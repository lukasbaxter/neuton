//! The launcher screen: version, account, and the server list.

use crate::auth_task::{SignIn, SignInTask};
use crate::icons::IconCache;
use crate::ping_task::{Ping, Pinger, Probe};
use crate::servers::{Server, ServerList};
use crate::theme::{self, ACCENT, CARD, DANGER, DIM, FG, LINE, LINE2, MID, WARN};
use egui::{Align, Color32, Frame, Layout, RichText, Stroke};
use neuton_auth::Accounts;
use neuton_net::{Resolution, ServerStatus};
use std::path::PathBuf;

/// Versions this build can play. One for now; the picker exists so adding a
/// second is a data change rather than a UI change.
const VERSIONS: &[&str] = &["26.2"];

/// Which modal, if any, is covering the list.
enum Modal {
    None,
    /// Adding a new server, or editing the one with this id.
    EditServer { id: Option<u64>, name: String, address: String, error: Option<String> },
    ConfirmRemoveServer { id: u64, name: String },
    ConfirmRemoveAccount { uuid: u128, name: String },
    Accounts,
}

pub struct Launcher {
    accounts: Accounts,
    accounts_path: PathBuf,
    servers: ServerList,
    signin: SignInTask,
    pinger: Pinger,
    icons: IconCache,
    version: String,
    selected: Option<u64>,
    modal: Modal,
    notice: Option<(String, Color32)>,
    /// Set once so the first paint kicks off a refresh of the whole list.
    pinged_at_startup: bool,
    /// Set when the user presses Join, and taken by the event loop, which owns
    /// the GPU and so is the only place a world can actually be started.
    pub pending_join: Option<PendingJoin>,
}

/// A join the launcher has asked for but cannot perform itself.
pub struct PendingJoin {
    pub host: String,
    pub port: u16,
    pub session: neuton_auth::Session,
}

impl Launcher {
    pub fn new() -> Self {
        let accounts_path =
            Accounts::default_path().unwrap_or_else(|_| PathBuf::from("accounts.json"));
        Self {
            accounts: Accounts::load(&accounts_path),
            accounts_path,
            servers: ServerList::load_default(),
            signin: SignInTask::default(),
            pinger: Pinger::default(),
            icons: IconCache::default(),
            version: VERSIONS[0].to_string(),
            selected: None,
            modal: Modal::None,
            notice: None,
            pinged_at_startup: false,
            pending_join: None,
        }
    }

    pub fn update(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();

        if self.signin.poll() {
            self.accounts = Accounts::load(&self.accounts_path);
        }
        if self.pinger.poll() {
            // A fresh status may carry a new icon, so drop the cached one.
            for s in self.servers.entries() {
                if let Ping::Ok(_) = self.pinger.state(s.id) {
                    self.icons.invalidate(s.id);
                }
            }
        }
        if !self.pinged_at_startup {
            self.pinged_at_startup = true;
            self.refresh_all();
        }

        egui::CentralPanel::default()
            .frame(Frame::new().fill(theme::BG).inner_margin(egui::Margin::same(0)))
            .show(ui, |ui| {
                self.top_bar(ui);
                Frame::new().inner_margin(egui::Margin::symmetric(20, 16)).show(ui, |ui| {
                    self.server_section(ui);
                    self.network_panel(ui);
                });
                self.bottom_bar(ui);
            });

        self.modal(&ctx);

        if self.signin.is_running() || self.pinger.any_pending() {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
    }

    fn refresh_all(&mut self) {
        self.pinger.retain(self.servers.entries());
        let ids: Vec<u64> = self.servers.entries().iter().map(|s| s.id).collect();
        self.icons.retain(&ids);
        for id in &ids {
            self.icons.invalidate(*id);
        }
        let entries: Vec<Server> = self.servers.entries().to_vec();
        self.pinger.refresh_all(&entries);
    }

    // ---------------------------------------------------------------- top bar

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        Frame::new()
            .fill(theme::RAISE)
            .stroke(Stroke::new(1.0, LINE))
            .inner_margin(egui::Margin::symmetric(20, 12))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(9.0, 9.0), egui::Sense::hover());
                    ui.painter().circle_filled(rect.center(), 4.5, ACCENT);
                    ui.add_space(2.0);
                    ui.label(RichText::new("neuton").size(18.0).strong().color(FG));

                    ui.add_space(14.0);
                    // One entry today. Kept as a picker so a second version is
                    // a data change, not a redesign.
                    egui::ComboBox::from_id_salt("version")
                        .selected_text(theme::mono(&self.version, FG).size(13.0))
                        .width(96.0)
                        .show_ui(ui, |ui| {
                            for v in VERSIONS {
                                ui.selectable_value(
                                    &mut self.version,
                                    (*v).to_string(),
                                    theme::mono(*v, FG).size(13.0),
                                );
                            }
                        });

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button(RichText::new("Accounts").size(13.0)).clicked() {
                            self.modal = Modal::Accounts;
                        }
                        match self.accounts.active() {
                            Some(a) => {
                                ui.label(RichText::new(&a.profile.name).size(13.5).color(FG));
                                let (r, _) = ui
                                    .allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                                ui.painter().circle_filled(
                                    r.center(),
                                    3.5,
                                    if a.is_valid() { ACCENT } else { WARN },
                                );
                            }
                            None => {
                                ui.label(RichText::new("not signed in").size(13.0).color(DIM));
                            }
                        }
                    });
                });
            });
    }

    // ----------------------------------------------------------- server list

    fn server_section(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(theme::mono("SERVERS", DIM).size(11.5));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button(RichText::new("Add server").size(12.5)).clicked() {
                    self.modal = Modal::EditServer {
                        id: None,
                        name: String::new(),
                        address: String::new(),
                        error: None,
                    };
                }
                let busy = self.pinger.any_pending();
                if ui
                    .add_enabled(!busy, egui::Button::new(RichText::new("Refresh").size(12.5)))
                    .clicked()
                {
                    self.refresh_all();
                }
                if busy {
                    ui.spinner();
                }
            });
        });
        ui.add_space(8.0);

        if self.servers.is_empty() {
            Frame::new()
                .fill(CARD)
                .stroke(Stroke::new(1.0, LINE))
                .corner_radius(10)
                .inner_margin(egui::Margin::same(28))
                .show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("No servers yet.").color(MID).size(14.5));
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new("Add one to see its MOTD, players and ping.")
                                .color(DIM)
                                .size(12.5),
                        );
                    });
                });
            return;
        }

        // Actions are collected and applied after the loop: the rows borrow the
        // list, and mutating it mid-iteration would not compile.
        let mut edit: Option<u64> = None;
        let mut remove: Option<u64> = None;
        let mut up: Option<u64> = None;
        let mut down: Option<u64> = None;
        let mut reping: Option<u64> = None;
        let mut join: Option<u64> = None;

        let entries: Vec<Server> = self.servers.entries().to_vec();
        let last = entries.len().saturating_sub(1);

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for (index, server) in entries.iter().enumerate() {
                let selected = self.selected == Some(server.id);
                let response = self.server_row(ui, server, selected, index, last, &mut |action| {
                    match action {
                        RowAction::Edit => edit = Some(server.id),
                        RowAction::Remove => remove = Some(server.id),
                        RowAction::Up => up = Some(server.id),
                        RowAction::Down => down = Some(server.id),
                        RowAction::Reping => reping = Some(server.id),
                        RowAction::Join => join = Some(server.id),
                    }
                });
                if response {
                    self.selected = Some(server.id);
                }
                ui.add_space(8.0);
            }
        });

        if let Some(id) = edit
            && let Some(s) = self.servers.get(id)
        {
            self.modal = Modal::EditServer {
                id: Some(id),
                name: s.name.clone(),
                address: s.address.clone(),
                error: None,
            };
        }
        if let Some(id) = remove
            && let Some(s) = self.servers.get(id)
        {
            self.modal =
                Modal::ConfirmRemoveServer { id, name: s.display_name().to_string() };
        }
        if let Some(id) = up {
            self.servers.move_up(id);
            let _ = self.servers.save();
        }
        if let Some(id) = down {
            self.servers.move_down(id);
            let _ = self.servers.save();
        }
        if let Some(id) = join {
            self.selected = Some(id);
            self.join_selected();
        }
        if let Some(id) = reping
            && let Some(s) = self.servers.get(id)
        {
            let (host, port) = s.host_port();
            let explicit = s.has_explicit_port();
            self.icons.invalidate(id);
            self.pinger.refresh_one(id, host, port, explicit);
        }
    }

    fn server_row(
        &mut self,
        ui: &mut egui::Ui,
        server: &Server,
        selected: bool,
        index: usize,
        last: usize,
        act: &mut dyn FnMut(RowAction),
    ) -> bool {
        let state = self.pinger.state(server.id).clone();
        let reachable = matches!(state, Ping::Ok(_));

        // The background senses clicks through UiBuilder rather than by calling
        // interact() on the finished frame. Interacting afterwards registers the
        // row on top of its own children, which silently swallows every button
        // and menu inside it.
        let scope = ui.scope_builder(
            egui::UiBuilder::new().sense(egui::Sense::click()),
            |ui| {
                let hovered = ui.response().hovered();
                let stroke = if selected {
                    Stroke::new(1.0, ACCENT)
                } else if hovered {
                    Stroke::new(1.0, LINE2)
                } else {
                    Stroke::new(1.0, LINE)
                };
                Frame::new()
                    .fill(if selected { theme::RAISE } else { CARD })
                    .stroke(stroke)
                    .corner_radius(10)
                    .inner_margin(egui::Margin::same(12))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            self.icon(ui, server.id, &state);
                            ui.add_space(10.0);

                            ui.vertical(|ui| {
                                ui.set_min_width(240.0);
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(server.display_name())
                                            .size(14.5)
                                            .strong()
                                            .color(FG),
                                    );
                                    ui.label(theme::mono(&server.address, DIM).size(11.0));
                                });
                                ui.add_space(2.0);
                                self.motd(ui, &state);
                            });

                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                // Right-to-left lays out in reverse, so the
                                // rightmost control is written first.
                                ui.menu_button(RichText::new("\u{22ee}").size(15.0), |ui| {
                                    if ui.button("Edit").clicked() {
                                        act(RowAction::Edit);
                                        ui.close();
                                    }
                                    if ui.button("Refresh").clicked() {
                                        act(RowAction::Reping);
                                        ui.close();
                                    }
                                    ui.separator();
                                    if ui
                                        .add_enabled(index > 0, egui::Button::new("Move up"))
                                        .clicked()
                                    {
                                        act(RowAction::Up);
                                        ui.close();
                                    }
                                    if ui
                                        .add_enabled(index < last, egui::Button::new("Move down"))
                                        .clicked()
                                    {
                                        act(RowAction::Down);
                                        ui.close();
                                    }
                                    ui.separator();
                                    if ui.button(RichText::new("Remove").color(DANGER)).clicked() {
                                        act(RowAction::Remove);
                                        ui.close();
                                    }
                                });
                                ui.add_space(4.0);

                                let can_join = reachable && self.accounts.active().is_some();
                                let join = ui.add_enabled(
                                    can_join,
                                    egui::Button::new(RichText::new("Join").size(13.0).strong()),
                                );
                                if join.clicked() {
                                    act(RowAction::Join);
                                }
                                if !can_join {
                                    join.on_disabled_hover_text(if !reachable {
                                        "server did not answer"
                                    } else {
                                        "sign in first"
                                    });
                                }

                                ui.add_space(6.0);
                                self.status_column(ui, &state);
                            });
                        });
                    });
            },
        );

        scope.response.clicked()
    }

    fn icon(&mut self, ui: &mut egui::Ui, id: u64, state: &Ping) {
        let size = egui::vec2(40.0, 40.0);
        let png = match state {
            Ping::Ok(probe) => probe.status.favicon_png.as_deref(),
            _ => None,
        };
        match self.icons.get(ui.ctx(), id, png) {
            Some(texture) => {
                ui.add(egui::Image::new(texture).fit_to_exact_size(size));
            }
            None => {
                let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                ui.painter().rect_filled(rect, 6.0, theme::RAISE);
                ui.painter().rect_stroke(
                    rect,
                    6.0,
                    Stroke::new(1.0, LINE2),
                    egui::StrokeKind::Inside,
                );
            }
        }
    }

    fn motd(&self, ui: &mut egui::Ui, state: &Ping) {
        match state {
            Ping::Ok(probe) => Self::motd_spans(ui, &probe.status),
            Ping::Pending => {
                ui.label(RichText::new("pinging...").color(DIM).size(12.5).italics());
            }
            Ping::Failed(why) => {
                ui.label(RichText::new(why).color(DANGER).size(12.5));
            }
            Ping::Unknown => {
                ui.label(RichText::new("not checked").color(DIM).size(12.5));
            }
        }
    }

    /// Draws the MOTD with the server's own colours, wrapping across its two
    /// lines the way the vanilla list does.
    fn motd_spans(ui: &mut egui::Ui, status: &ServerStatus) {
        if status.motd.is_empty() {
            ui.label(RichText::new("no MOTD").color(DIM).size(12.5));
            return;
        }
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            for span in &status.motd {
                for (i, line) in span.text.split('\n').enumerate() {
                    if i > 0 {
                        ui.end_row();
                    }
                    if line.is_empty() {
                        continue;
                    }
                    let mut text = RichText::new(line).size(12.5);
                    text = match span.color {
                        Some([r, g, b]) => text.color(Color32::from_rgb(r, g, b)),
                        None => text.color(MID),
                    };
                    if span.bold {
                        text = text.strong();
                    }
                    if span.italic {
                        text = text.italics();
                    }
                    if span.underlined {
                        text = text.underline();
                    }
                    if span.strikethrough {
                        text = text.strikethrough();
                    }
                    ui.label(text);
                }
            }
        });
    }

    fn status_column(&self, ui: &mut egui::Ui, state: &Ping) {
        match state {
            Ping::Ok(probe) => {
                let status = &probe.status;
                ui.vertical(|ui| {
                    ui.with_layout(Layout::top_down(Align::Max), |ui| {
                        let bad = !status.compatible();
                        ui.label(
                            theme::mono(format!("{:.0} ms", status.latency_ms), latency_color(status.latency_ms))
                                .size(12.0),
                        );
                        if status.players_max >= 0 {
                            ui.label(
                                theme::mono(
                                    format!("{}/{}", status.players_online, status.players_max),
                                    MID,
                                )
                                .size(12.0),
                            );
                        }
                        if bad {
                            ui.label(
                                RichText::new(format!("needs {}", status.version_name))
                                    .color(WARN)
                                    .size(11.0),
                            )
                            .on_hover_text(format!(
                                "server speaks protocol {}, this build speaks {}",
                                status.protocol,
                                neuton_protocol::PROTOCOL_VERSION
                            ));
                        }
                    });
                });
            }
            Ping::Pending => {
                ui.spinner();
            }
            _ => {
                ui.label(theme::mono("--", DIM).size(12.0));
            }
        }
    }

    /// Hands a join to the event loop, which owns the window and the GPU.
    fn join_selected(&mut self) {
        let Some(server) = self.selected.and_then(|id| self.servers.get(id)) else {
            self.notice = Some(("Select a server first.".to_string(), WARN));
            return;
        };
        let Some(account) = self.accounts.active().cloned() else {
            self.notice = Some(("Sign in first.".to_string(), WARN));
            return;
        };
        // The address is resolved again by the connection, including any SRV
        // record, so the typed form is what gets passed on.
        let (host, port) = server.host_port();
        self.notice = None;
        self.pending_join = Some(PendingJoin { host, port, session: account });
    }

    // -------------------------------------------------------- network detail

    /// Everything the launcher learned about reaching the selected server.
    ///
    /// Consolidated here rather than sprinkled through the row, because the row
    /// has to stay scannable and this is the panel you open when a server is
    /// behaving oddly.
    fn network_panel(&self, ui: &mut egui::Ui) {
        let Some(id) = self.selected else { return };
        let Some(server) = self.servers.get(id) else { return };
        let Ping::Ok(probe) = self.pinger.state(id) else { return };
        let Probe { status, resolution } = probe.as_ref();

        ui.add_space(14.0);
        ui.label(theme::mono("NETWORK", DIM).size(11.5));
        ui.add_space(8.0);

        Frame::new()
            .fill(CARD)
            .stroke(Stroke::new(1.0, LINE))
            .corner_radius(10)
            .inner_margin(egui::Margin::same(14))
            .show(ui, |ui| {
                egui::Grid::new("net-detail")
                    .num_columns(2)
                    .spacing([18.0, 6.0])
                    .min_col_width(120.0)
                    .show(ui, |ui| {
                        Self::field(ui, "typed", &server.address);

                        if let Some(srv) = &resolution.srv {
                            Self::field(
                                ui,
                                "srv record",
                                &format!(
                                    "{}:{}  priority {} weight {}",
                                    srv.target.trim_end_matches('.'),
                                    srv.port,
                                    srv.priority,
                                    srv.weight
                                ),
                            );
                        }
                        Self::field(
                            ui,
                            "connects to",
                            &format!("{}:{}", resolution.effective_host, resolution.effective_port),
                        );

                        let addrs = if resolution.addresses.is_empty() {
                            "none".to_string()
                        } else {
                            resolution
                                .addresses
                                .iter()
                                .map(|a| a.to_string())
                                .collect::<Vec<_>>()
                                .join(", ")
                        };
                        Self::field(ui, "addresses", &addrs);

                        if let Some(rev) = &resolution.reverse {
                            Self::field(ui, "reverse dns", rev);
                        }

                        Self::field(
                            ui,
                            "timings",
                            &format!(
                                "dns {:.0} ms · tcp {:.0} ms · status {:.0} ms · rtt {:.0} ms",
                                resolution.lookup_ms.unwrap_or(0.0),
                                status.connect_ms,
                                status.status_ms,
                                status.latency_ms
                            ),
                        );
                        Self::field(
                            ui,
                            "server",
                            &format!("{} · protocol {}", status.version_name, status.protocol),
                        );
                    });

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(8.0);
                ui.label(theme::mono("what that tells you", DIM).size(10.5));
                ui.add_space(6.0);
                for fact in Self::facts(status, resolution) {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        ui.label(theme::mono("·", ACCENT).size(12.0));
                        ui.label(RichText::new(fact).color(MID).size(12.5));
                    });
                }
            });
    }

    fn field(ui: &mut egui::Ui, label: &str, value: &str) {
        ui.label(theme::mono(label, DIM).size(12.0));
        ui.label(theme::mono(value, FG).size(12.0));
        ui.end_row();
    }

    /// Observations drawn from the numbers above.
    ///
    /// These are inferences, not measurements, and are worded that way.
    fn facts(status: &ServerStatus, resolution: &Resolution) -> Vec<String> {
        let mut out = Vec::new();

        // Light in fibre travels at roughly two thirds of c, so half the round
        // trip puts a hard ceiling on how far away the machine can be. Anything
        // under that is impossible; well over it means queueing or routing.
        if status.latency_ms > 0.0 {
            let km = (status.latency_ms / 2.0) * 200.0;
            out.push(format!(
                "at {:.0} ms round trip the server is at most ~{:.0} km away through fibre, and probably much closer",
                status.latency_ms, km
            ));
        }

        // The application ping and the TCP handshake cross the same path once
        // each. A large gap is the server thread, not the network.
        let gap = status.latency_ms - status.connect_ms;
        if status.connect_ms > 0.0 && gap.abs() > 15.0 {
            if gap > 0.0 {
                out.push(format!(
                    "the ping takes {:.0} ms longer than the TCP handshake over the same path, so that time is the server thinking rather than the network",
                    gap
                ));
            } else {
                out.push(format!(
                    "the handshake took {:.0} ms longer than the ping, which usually means the first connection paid for a route or TLS setup the second reused",
                    -gap
                ));
            }
        }

        if resolution.redirected() {
            out.push(
                "an SRV record points this domain somewhere else, which is how a server runs on a non-default port without anyone typing one".to_string(),
            );
        }

        if resolution.addresses.len() > 1 {
            out.push(format!(
                "{} addresses answer for this name, so something is load balancing or it sits behind a proxy network",
                resolution.addresses.len()
            ));
        }

        if let Some(rev) = &resolution.reverse {
            let lower = rev.to_ascii_lowercase();
            for (needle, who) in [
                ("cloudflare", "Cloudflare"),
                ("tcpshield", "TCPShield"),
                ("ovh", "OVH"),
                ("hetzner", "Hetzner"),
                ("amazonaws", "AWS"),
                ("googleusercontent", "Google Cloud"),
                ("azure", "Azure"),
                ("digitalocean", "DigitalOcean"),
            ] {
                if lower.contains(needle) {
                    out.push(format!(
                        "reverse DNS points at {who}, so you are talking to their edge rather than the machine running the game"
                    ));
                    break;
                }
            }
        } else if !resolution.addresses.is_empty() {
            out.push(
                "the address has no reverse DNS, which is normal for consumer connections and for hosts behind a proxy".to_string(),
            );
        }

        if resolution.addresses.iter().any(|a| a.is_ipv6()) {
            out.push("the server also answers on IPv6".to_string());
        }

        if status.payload_bytes > 0 {
            out.push(format!(
                "the status reply was {} bytes, {}",
                status.payload_bytes,
                if status.favicon_png.is_some() {
                    "most of it the server icon"
                } else {
                    "with no server icon in it"
                }
            ));
        }

        if !status.compatible() {
            out.push(format!(
                "this build speaks protocol {} and the server speaks {}, so joining would be refused at the handshake",
                neuton_protocol::PROTOCOL_VERSION,
                status.protocol
            ));
        }

        out
    }

    // ------------------------------------------------------------ bottom bar

    fn bottom_bar(&mut self, ui: &mut egui::Ui) {
        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            Frame::new()
                .fill(theme::RAISE)
                .stroke(Stroke::new(1.0, LINE))
                .inner_margin(egui::Margin::symmetric(20, 12))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        // Copied out so the Play handler can take &mut self.
                        let chosen: Option<String> = self
                            .selected
                            .and_then(|id| self.servers.get(id))
                            .map(|s| s.display_name().to_string());
                        let ready = chosen.is_some() && self.accounts.active().is_some();

                        if ui
                            .add_enabled(
                                ready,
                                egui::Button::new(RichText::new("Play").size(14.5).strong()),
                            )
                            .clicked()
                        {
                            self.join_selected();
                        }

                        match &chosen {
                            Some(name) => {
                                ui.label(RichText::new(name).color(MID).size(13.0))
                            }
                            None => ui.label(
                                RichText::new("select a server").color(DIM).size(13.0),
                            ),
                        };

                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if let Some((msg, colour)) = &self.notice {
                                ui.label(RichText::new(msg).color(*colour).size(12.5));
                            }
                        });
                    });
                });
        });
    }

    // ---------------------------------------------------------------- modals

    fn modal(&mut self, ctx: &egui::Context) {
        match &self.modal {
            Modal::None => {}
            Modal::EditServer { .. } => self.edit_server_modal(ctx),
            Modal::ConfirmRemoveServer { .. } => self.remove_server_modal(ctx),
            Modal::Accounts => self.accounts_modal(ctx),
            Modal::ConfirmRemoveAccount { .. } => self.remove_account_modal(ctx),
        }
    }

    fn window(title: &str) -> egui::Window<'static> {
        egui::Window::new(title.to_string())
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .frame(
                Frame::new()
                    .fill(theme::RAISE)
                    .stroke(Stroke::new(1.0, LINE2))
                    .corner_radius(12)
                    .inner_margin(egui::Margin::same(18)),
            )
    }

    /// True if the user asked to dismiss whatever is open, by Escape.
    ///
    /// Every modal honours it. A dialog you cannot get out of without finding
    /// the right button is the kind of thing that makes an app feel broken.
    fn dismissed(ctx: &egui::Context) -> bool {
        ctx.input(|i| i.key_pressed(egui::Key::Escape))
    }

    fn edit_server_modal(&mut self, ctx: &egui::Context) {
        let Modal::EditServer { id, mut name, mut address, mut error } =
            std::mem::replace(&mut self.modal, Modal::None)
        else {
            return;
        };
        let editing = id.is_some();
        let mut keep_open = !Self::dismissed(ctx);
        let mut commit = false;
        let mut open = true;

        Self::window(if editing { "Edit server" } else { "Add server" })
            .open(&mut open)
            .show(ctx, |ui| {
            ui.set_width(360.0);
            ui.label(RichText::new("Name").color(MID).size(12.5));
            ui.add(
                egui::TextEdit::singleline(&mut name)
                    .hint_text("optional")
                    .desired_width(f32::INFINITY),
            );
            ui.add_space(10.0);
            ui.label(RichText::new("Address").color(MID).size(12.5));
            let addr_field = ui.add(
                egui::TextEdit::singleline(&mut address)
                    .hint_text("play.example.com")
                    .font(egui::TextStyle::Monospace)
                    .desired_width(f32::INFINITY),
            );
            ui.label(
                RichText::new("Port defaults to 25565.").color(DIM).size(11.5),
            );

            if let Some(msg) = &error {
                ui.add_space(8.0);
                ui.label(RichText::new(msg).color(DANGER).size(12.5));
            }

            ui.add_space(14.0);
            ui.horizontal(|ui| {
                let save = ui.button(RichText::new(if editing { "Save" } else { "Add" }).size(13.0));
                let enter = addr_field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if save.clicked() || enter {
                    commit = true;
                }
                if ui.button(RichText::new("Cancel").size(13.0)).clicked() {
                    keep_open = false;
                }
            });
        });

        if commit {
            if address.trim().is_empty() {
                error = Some("An address is required.".into());
            } else {
                match id {
                    Some(id) => {
                        self.servers.edit(id, name.trim(), address.trim());
                        self.icons.invalidate(id);
                        let (h, p, explicit) = self
                            .servers
                            .get(id)
                            .map(|s| (s.host_port().0, s.host_port().1, s.has_explicit_port()))
                            .unwrap_or_default();
                        self.pinger.refresh_one(id, h, p, explicit);
                    }
                    None => {
                        let new_id = self.servers.add(name.trim(), address.trim());
                        if let Some(s) = self.servers.get(new_id) {
                            let (h, p) = s.host_port();
                            self.pinger.refresh_one(new_id, h, p, s.has_explicit_port());
                        }
                        self.selected = Some(new_id);
                    }
                }
                if let Err(e) = self.servers.save() {
                    self.notice = Some((format!("Could not save the server list: {e}"), DANGER));
                }
                keep_open = false;
            }
        }

        if keep_open && open {
            self.modal = Modal::EditServer { id, name, address, error };
        }
    }

    fn remove_server_modal(&mut self, ctx: &egui::Context) {
        let Modal::ConfirmRemoveServer { id, name } =
            std::mem::replace(&mut self.modal, Modal::None)
        else {
            return;
        };
        let mut keep = !Self::dismissed(ctx);
        let mut open = true;
        Self::window("Remove server").open(&mut open).show(ctx, |ui| {
            ui.set_width(320.0);
            ui.label(RichText::new(format!("Remove {name} from the list?")).color(MID).size(13.5));
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                if ui.button(RichText::new("Remove").color(DANGER).size(13.0)).clicked() {
                    self.servers.remove(id);
                    let _ = self.servers.save();
                    self.icons.invalidate(id);
                    if self.selected == Some(id) {
                        self.selected = None;
                    }
                    keep = false;
                }
                if ui.button(RichText::new("Cancel").size(13.0)).clicked() {
                    keep = false;
                }
            });
        });
        if keep && open {
            self.modal = Modal::ConfirmRemoveServer { id, name };
        }
    }

    fn accounts_modal(&mut self, ctx: &egui::Context) {
        // Taken out first. Leaving it set and only re-setting it on the
        // keep-open path meant Close had nothing to clear, so the dialog could
        // never be dismissed.
        self.modal = Modal::None;

        let mut keep = !Self::dismissed(ctx);
        let mut switch_to: Option<String> = None;
        let mut remove: Option<(u128, String)> = None;
        let mut open = true;

        Self::window("Accounts").open(&mut open).show(ctx, |ui| {
            ui.set_width(420.0);
            if self.accounts.is_empty() {
                ui.label(RichText::new("No accounts signed in.").color(MID).size(14.0));
                ui.add_space(6.0);
            }

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

            for (uuid, name, uuid_text, valid, active) in &rows {
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                    ui.painter().circle_filled(r.center(), 3.5, if *active { ACCENT } else { LINE2 });
                    ui.vertical(|ui| {
                        ui.label(RichText::new(name).size(14.0).strong().color(FG));
                        ui.label(theme::mono(uuid_text, DIM).size(10.5));
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button(RichText::new("Sign out").size(12.0).color(DANGER)).clicked() {
                            remove = Some((*uuid, name.clone()));
                        }
                        if !*active && ui.button(RichText::new("Use").size(12.0)).clicked() {
                            switch_to = Some(name.clone());
                        }
                        if !*valid {
                            ui.label(RichText::new("needs refresh").color(WARN).size(11.0));
                        }
                    });
                });
                ui.add_space(6.0);
            }

            ui.separator();
            ui.add_space(6.0);
            self.sign_in_controls(ui);
            ui.add_space(10.0);
            if ui.button(RichText::new("Close").size(13.0)).clicked() {
                keep = false;
            }
        });

        if let Some(name) = switch_to {
            self.accounts.set_active(&name);
            let _ = self.accounts.save();
        }
        if let Some((uuid, name)) = remove {
            self.modal = Modal::ConfirmRemoveAccount { uuid, name };
            return;
        }
        if keep && open {
            self.modal = Modal::Accounts;
        }
    }

    fn remove_account_modal(&mut self, ctx: &egui::Context) {
        let Modal::ConfirmRemoveAccount { uuid, name } =
            std::mem::replace(&mut self.modal, Modal::None)
        else {
            return;
        };
        let mut decided = Self::dismissed(ctx);
        let mut open = true;
        Self::window("Sign out").open(&mut open).show(ctx, |ui| {
            ui.set_width(320.0);
            ui.label(
                RichText::new(format!("Sign {name} out of this computer?")).color(MID).size(13.5),
            );
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                if ui.button(RichText::new("Sign out").color(DANGER).size(13.0)).clicked() {
                    self.accounts.remove(&name);
                    let _ = self.accounts.save();
                    decided = true;
                }
                if ui.button(RichText::new("Cancel").size(13.0)).clicked() {
                    decided = true;
                }
            });
        });
        if decided || !open {
            // Cancelling a sign-out goes back to the account list rather than
            // closing everything, since that is where the user came from.
            self.modal = Modal::Accounts;
        } else {
            self.modal = Modal::ConfirmRemoveAccount { uuid, name };
        }
    }

    fn sign_in_controls(&mut self, ui: &mut egui::Ui) {
        match self.signin.state.clone() {
            SignIn::Idle => {
                if ui.button(RichText::new("Add account").size(13.0)).clicked() {
                    self.signin.start(self.accounts_path.clone());
                }
            }
            SignIn::Starting => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("Contacting Microsoft...").color(MID).size(13.0));
                });
            }
            SignIn::Waiting { code, url } => {
                Frame::new()
                    .fill(CARD)
                    .stroke(Stroke::new(1.0, ACCENT.gamma_multiply(0.45)))
                    .corner_radius(8)
                    .inner_margin(egui::Margin::same(13))
                    .show(ui, |ui| {
                        ui.label(RichText::new("Enter this code to sign in").color(MID).size(12.5));
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&code).monospace().size(24.0).strong().color(ACCENT));
                            ui.add_space(8.0);
                            if ui.button(RichText::new("Copy").size(12.0)).clicked() {
                                ui.ctx().copy_text(code.clone());
                            }
                        });
                        ui.add_space(4.0);
                        ui.hyperlink_to(theme::mono(&url, MID).size(12.0), &url);
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(RichText::new("Waiting for approval").color(DIM).size(12.0));
                        });
                    });
            }
            SignIn::Done { name } => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("Signed in as {name}")).color(ACCENT).size(13.0));
                    if ui.button(RichText::new("OK").size(12.0)).clicked() {
                        self.signin.dismiss();
                    }
                });
            }
            SignIn::Failed(why) => {
                Frame::new()
                    .fill(CARD)
                    .stroke(Stroke::new(1.0, WARN.gamma_multiply(0.5)))
                    .corner_radius(8)
                    .inner_margin(egui::Margin::same(12))
                    .show(ui, |ui| {
                        ui.label(RichText::new("Could not sign in").color(WARN).size(13.0).strong());
                        ui.add_space(4.0);
                        ui.label(RichText::new(&why).color(MID).size(12.0));
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.button(RichText::new("Try again").size(12.0)).clicked() {
                                self.signin.dismiss();
                                self.signin.start(self.accounts_path.clone());
                            }
                            if ui.button(RichText::new("Dismiss").size(12.0)).clicked() {
                                self.signin.dismiss();
                            }
                        });
                    });
            }
        }
    }
}

enum RowAction {
    Edit,
    Remove,
    Up,
    Down,
    Reping,
    Join,
}

/// Green through amber to red, on the same thresholds the vanilla list uses.
fn latency_color(ms: f64) -> Color32 {
    if ms < 75.0 {
        ACCENT
    } else if ms < 150.0 {
        Color32::from_rgb(0x8f, 0xd6, 0x6a)
    } else if ms < 300.0 {
        WARN
    } else {
        DANGER
    }
}

impl Default for Launcher {
    fn default() -> Self {
        Self::new()
    }
}
