//! The Mods window: the Workshop mods Steam installed, with their size, when they were last
//! updated, and which of the user's servers (favorite, recent, watched, or added) use them;
//! unsubscribing from them (through the Workshop helper); and the folders Steam left behind,
//! which can be deleted.

use eframe::egui::{self, Align, Layout, RichText, Sense};
use egui_extras::{Column, TableBuilder};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::{App, ERROR, FAIR, Fetch, GOOD, Msg, WARN, join, open_workshop};
use crate::inventory::{self, Folder, Inventory, Leftover};
use crate::launch;
use crate::mods::ModState;
use crate::server::ModRef;
use crate::workshop::{self, Event, ItemState};

/// The Mods window's id, to bring it to the front.
const WINDOW: &str = "mods-window";
/// After unsubscribing, look again this long after the helper quits: Steam deletes the files a
/// little after it stops counting DayZ as running.
const RESCANS: [Duration; 4] = [
    Duration::from_secs(1),
    Duration::from_secs(5),
    Duration::from_secs(15),
    Duration::from_secs(40),
];

pub enum PageMsg {
    Scanned(Inventory),
    /// A leftover folder was deleted (the bytes freed), or couldn't be.
    Deleted(u64, Result<u64, String>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Installed,
    Leftovers,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sort {
    Name,
    Size,
    Updated,
    UsedBy,
}

/// A step that needs a second click.
enum Confirm {
    Unsubscribe(Vec<u64>),
    Delete(u64),
}

/// One run of the Workshop helper, unsubscribing. Its events carry its number (shared with the
/// join window's runs).
struct Run {
    id: u64,
    /// Kept for its lifetime: dropping it stops the helper.
    _helper: workshop::Helper,
    connected: bool,
    items: HashMap<u64, (ItemState, Option<String>)>,
    error: Option<String>,
    finished: bool,
}

pub struct ModsPage {
    /// Closed while still busy (unsubscribing or deleting): kept until that's done.
    open: bool,
    focus: bool,
    tab: Tab,
    inventory: Option<Inventory>,
    scanning: bool,
    scan_again: bool,
    /// Scans still to make after unsubscribing.
    rescans: Vec<Instant>,
    search: String,
    sort: Sort,
    descending: bool,
    selected: BTreeSet<u64>,
    confirm: Option<Confirm>,
    run: Option<Run>,
    /// Mods unsubscribed from while the page was open, until Steam deletes them.
    unsubscribed: HashSet<u64>,
    deleting: Option<u64>,
    message: Option<(String, bool)>,
}

impl ModsPage {
    fn new() -> Self {
        Self {
            open: true,
            focus: false,
            tab: Tab::Installed,
            inventory: None,
            scanning: false,
            scan_again: false,
            rescans: Vec::new(),
            search: String::new(),
            sort: Sort::Name,
            descending: false,
            selected: BTreeSet::new(),
            confirm: None,
            run: None,
            unsubscribed: HashSet::new(),
            deleting: None,
            message: None,
        }
    }

    fn busy(&self) -> bool {
        self.deleting.is_some() || self.run.as_ref().is_some_and(|r| !r.finished)
    }
}

/// What "your servers" are, for hover texts.
const YOUR_SERVERS: &str = "Your servers are your favorites, recent and watched servers, and \
                            servers added by address. Only those whose mods are known count.";

/// The user's servers (favorite, recent, watched, and added), and which mods each uses.
struct Usage {
    /// Each server's name, and its mods if it's known which.
    servers: Vec<(String, Option<Vec<ModRef>>)>,
    /// For each mod, the servers (places in `servers`) that use it.
    by_mod: HashMap<u64, Vec<usize>>,
    /// Servers still being asked for their mods.
    asking: usize,
}

impl Usage {
    fn unknown(&self) -> Vec<&str> {
        self.servers
            .iter()
            .filter(|(_, mods)| mods.is_none())
            .map(|(name, _)| name.as_str())
            .collect()
    }

    fn users(&self, id: u64) -> &[usize] {
        self.by_mod.get(&id).map_or(&[], Vec::as_slice)
    }
}

/// One row of the installed mods' table.
struct Row<'a> {
    folder: &'a Folder,
    name: String,
    /// The Workshop's name for it, when it differs from the shown one.
    title: Option<&'a str>,
    outdated: bool,
    users: &'a [usize],
}

enum Action {
    OpenPage(u64),
    OpenFolder(PathBuf),
    Ask(Confirm),
    Cancel,
    Unsubscribe(Vec<u64>),
    Delete(u64),
    Rescan,
}

impl App {
    /// Opens the Mods window (or brings it to the front), and looks at the Workshop folder again.
    pub(super) fn open_mods_page(&mut self) {
        match &mut self.mods_page {
            Some(page) => {
                page.open = true;
                page.focus = true;
            }
            None => self.mods_page = Some(ModsPage::new()),
        }
        self.mods_scan();
        // Which mods the favorite and recent servers use, where that isn't known yet.
        for addr in self.usage_addrs() {
            if self.known_mods(&addr).is_none() {
                self.request_rules(&addr);
            }
        }
    }

    /// The user's servers, whose mods count as used: favorites first, then recent, watched, and
    /// added servers, each once.
    fn usage_addrs(&self) -> Vec<String> {
        let mut seen = HashSet::new();
        self.state
            .favorites
            .iter()
            .chain(self.state.recent.iter().map(|r| &r.addr))
            .chain(self.state.watched.iter().map(|w| &w.addr))
            .chain(&self.state.added)
            .filter(|a| seen.insert(a.as_str()))
            .cloned()
            .collect()
    }

    /// A server's mods, if known: what it said itself, else the list's.
    fn known_mods(&self, addr: &str) -> Option<Vec<ModRef>> {
        if let Some(Fetch::Done(rules)) = self.rules.get(addr) {
            return Some(rules.mods.clone());
        }
        let &i = self.index.get(addr)?;
        self.mods_for(&self.servers[i]).0
    }

    fn usage(&self) -> Usage {
        let servers: Vec<(String, Option<Vec<ModRef>>)> = self
            .usage_addrs()
            .into_iter()
            .map(|addr| {
                let name = match self.index.get(&addr) {
                    Some(&i) => self.servers[i].name.clone(),
                    None => {
                        let recent = self.state.recent.iter().find(|r| r.addr == addr);
                        let watched = self.state.watched.iter().find(|w| w.addr == addr);
                        recent
                            .map(|r| r.name.clone())
                            .filter(|n| !n.is_empty())
                            .or_else(|| watched.map(|w| w.name.clone()))
                            .filter(|n| !n.is_empty())
                            .unwrap_or_else(|| addr.clone())
                    }
                };
                (name, self.known_mods(&addr))
            })
            .collect();
        let lists: Vec<Vec<u64>> = servers
            .iter()
            .map(|(_, mods)| mods.iter().flatten().map(|m| m.id).collect::<Vec<u64>>())
            .collect();
        let asking = self
            .usage_addrs()
            .iter()
            .filter(|a| matches!(self.rules.get(a.as_str()), Some(Fetch::Loading)))
            .count();
        Usage {
            by_mod: inventory::used_by(&lists),
            servers,
            asking,
        }
    }

    /// Reads the Workshop folder again, in the background.
    fn mods_scan(&mut self) {
        if let Some(mut page) = self.mods_page.take() {
            self.start_scan(&mut page);
            self.mods_page = Some(page);
        }
    }

    fn start_scan(&self, page: &mut ModsPage) {
        let Ok(install) = &self.install else { return };
        let install = install.clone();
        if page.scanning {
            page.scan_again = true;
            return;
        }
        page.scanning = true;
        self.spawn(move || {
            let account = inventory::current_account(&install);
            Msg::Mods(PageMsg::Scanned(inventory::scan(&install, account)))
        });
    }

    pub(super) fn mods_msg(&mut self, msg: PageMsg) {
        match msg {
            PageMsg::Scanned(found) => {
                let ids: Vec<u64> = found
                    .installed
                    .iter()
                    .chain(&found.leftovers)
                    .map(|f| f.id)
                    .collect();
                let Some(page) = self.mods_page.as_mut() else {
                    return;
                };
                page.scanning = false;
                let installed: HashSet<u64> = found.installed.iter().map(|f| f.id).collect();
                page.unsubscribed.retain(|id| installed.contains(id));
                page.selected.retain(|id| installed.contains(id));
                page.inventory = Some(found);
                let again = std::mem::take(&mut page.scan_again);
                self.reload_local();
                // Names and update times from the Workshop.
                self.request_details(&ids);
                if again {
                    self.mods_scan();
                }
            }
            PageMsg::Deleted(id, result) => {
                let text = match &result {
                    Ok(freed) => (
                        format!("Deleted folder {id} ({})", size_text(*freed)),
                        false,
                    ),
                    Err(e) => (format!("Couldn't delete folder {id}: {e}"), true),
                };
                let Some(page) = self.mods_page.as_mut() else {
                    self.toast(text.0);
                    return;
                };
                page.deleting = None;
                let hidden = !page.open;
                page.message = Some(text.clone());
                if hidden {
                    self.toast(text.0);
                }
                self.reload_local();
                self.mods_scan();
            }
        }
    }

    /// An event from a Workshop helper run (only the Mods page's own runs are taken).
    pub(super) fn mods_workshop_event(&mut self, run: u64, event: &Event) {
        let Some(page) = self.mods_page.as_mut() else {
            return;
        };
        let Some(r) = page.run.as_mut().filter(|r| r.id == run) else {
            return;
        };
        match event {
            Event::Ready => r.connected = true,
            Event::Item {
                id, state, message, ..
            } => {
                r.items.insert(*id, (*state, message.clone()));
                if *state == ItemState::Unsubscribed {
                    page.unsubscribed.insert(*id);
                }
            }
            Event::Error { message } => r.error = Some(message.clone()),
            Event::Done => {}
            Event::Exited => {
                r.finished = true;
                if !r.connected && r.error.is_none() {
                    r.error = Some("The Workshop helper stopped before reaching Steam".into());
                }
                let count = |state| r.items.values().filter(|(s, _)| *s == state).count();
                let (done, failed, unanswered) = (
                    count(ItemState::Unsubscribed),
                    count(ItemState::Failed),
                    count(ItemState::Unsubscribing),
                );
                page.message = Some(match &r.error {
                    Some(e) => (e.clone(), true),
                    None => run_summary(done, failed, unanswered),
                });
                let now = Instant::now();
                page.rescans = RESCANS.iter().map(|d| now + *d).collect();
                if !page.open
                    && let Some((text, _)) = page.message.clone()
                {
                    self.toast(text);
                }
            }
        }
    }

    /// Why unsubscribing and deleting can't be done now, if they can't.
    fn mods_blocked(&self, page: &ModsPage) -> Option<&'static str> {
        if launch::game_running_lately() {
            Some("Close DayZ first")
        } else if page.deleting.is_some() {
            Some("Wait for the folder to be deleted")
        } else if workshop::running() {
            Some("Wait for the Workshop helper to finish")
        } else {
            None
        }
    }

    pub(super) fn mods_window(&mut self, ctx: &egui::Context) {
        let Some(mut page) = self.mods_page.take() else {
            return;
        };
        if !page.open {
            // Kept only while it's still working.
            if page.busy() {
                self.mods_page = Some(page);
            }
            return;
        }
        let now = Instant::now();
        if page.rescans.first().is_some_and(|t| *t <= now) {
            page.rescans.remove(0);
            self.start_scan(&mut page);
        }
        let mut action: Option<Action> = None;
        let usage = self.usage();
        let blocked = self.mods_blocked(&page);
        let id = egui::Id::new(WINDOW);
        if std::mem::take(&mut page.focus) {
            super::windows::to_front(ctx, id);
        }
        let mut open = true;
        super::windows::placed(egui::Window::new("Mods"), ctx, id, 900.0)
            .collapsible(false)
            .resizable(true)
            .default_size([900.0, 540.0])
            .min_width(780.0)
            .min_height(300.0)
            .open(&mut open)
            .show(ctx, |ui| {
                if let Err(e) = &self.install {
                    ui.label(RichText::new(super::sentence(e)).color(ERROR));
                    return;
                }
                let Some(inventory) = page.inventory.take() else {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Looking at the Workshop folder…");
                    });
                    return;
                };
                self.mods_header(ui, &mut page, &inventory, &mut action);
                ui.separator();
                match page.tab {
                    Tab::Installed => {
                        self.installed_tab(ui, &mut page, &inventory, &usage, blocked, &mut action)
                    }
                    Tab::Leftovers => {
                        leftovers_tab(ui, &mut page, &inventory, blocked, &mut action)
                    }
                }
                page.inventory = Some(inventory);
            });
        // Not while it's working on the mods.
        if super::windows::escape_closes(ctx, id) && !page.busy() {
            open = false;
        }
        if !open {
            page.open = false;
            page.confirm = None;
        }
        if action.is_none() {
            self.confirm_modal(ctx, &page, &usage, blocked, &mut action);
        }
        if let Some(action) = action {
            self.mods_action(&mut page, action);
        }
        if page.busy() || page.scanning || !page.rescans.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
        self.mods_page = Some(page);
    }

    fn mods_header(
        &self,
        ui: &mut egui::Ui,
        page: &mut ModsPage,
        inventory: &Inventory,
        action: &mut Option<Action>,
    ) {
        ui.horizontal(|ui| {
            let total = |folders: &[Folder]| folders.iter().map(|f| f.size).sum::<u64>();
            ui.selectable_value(
                &mut page.tab,
                Tab::Installed,
                format!(
                    "Installed ({} · {})",
                    inventory.installed.len(),
                    size_text(total(&inventory.installed))
                ),
            );
            let leftovers = if inventory.leftovers.is_empty() {
                "Leftover folders (none)".to_string()
            } else {
                format!(
                    "Leftover folders ({} · {})",
                    inventory.leftovers.len(),
                    size_text(total(&inventory.leftovers))
                )
            };
            ui.selectable_value(&mut page.tab, Tab::Leftovers, leftovers)
                .on_hover_text("Folders in the Workshop folder that Steam didn't install");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let rescan = ui
                    .add_enabled(!page.scanning, egui::Button::new("⟳ Look again"))
                    .on_hover_text("Read the Workshop folder and Steam's records again");
                if rescan.clicked() {
                    *action = Some(Action::Rescan);
                }
                if page.scanning {
                    ui.spinner();
                }
            });
        });
    }

    fn installed_tab(
        &self,
        ui: &mut egui::Ui,
        page: &mut ModsPage,
        inventory: &Inventory,
        usage: &Usage,
        blocked: Option<&'static str>,
        action: &mut Option<Action>,
    ) {
        let rows = self.installed_rows(page, inventory, usage);
        ui.horizontal(|ui| {
            ui.label("Search");
            ui.add(
                egui::TextEdit::singleline(&mut page.search)
                    .hint_text("name or Workshop id")
                    .desired_width(220.0),
            );
            if !page.search.trim().is_empty() {
                ui.label(
                    RichText::new(format!(
                        "{} of {} mods",
                        rows.len(),
                        inventory.installed.len()
                    ))
                    .weak(),
                );
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let unknown = usage.unknown();
                if usage.asking > 0 {
                    ui.label(
                        RichText::new(format!("Asking {} servers for their mods…", usage.asking))
                            .weak(),
                    );
                    ui.spinner();
                } else if !unknown.is_empty() {
                    let text = if unknown.len() == 1 {
                        "1 of your servers didn't say which mods it uses".to_string()
                    } else {
                        format!(
                            "{} of your servers didn't say which mods they use",
                            unknown.len()
                        )
                    };
                    ui.label(RichText::new(text).color(WARN))
                        .on_hover_text(format!("{}\n\n{YOUR_SERVERS}", names_list(&unknown)));
                }
            });
        });
        if inventory.no_records {
            ui.label(
                RichText::new(
                    "Steam's record of installed mods (appworkshop_221100.acf) couldn't be read, \
                     so sizes are counted here and leftover folders can't be told apart.",
                )
                .color(WARN),
            );
        }
        let helper = workshop::helper().is_some();
        // The selection bar and messages first, at the bottom, so the table takes what's left.
        bottom_panel("mods-installed-bottom").show(ui, |ui| {
            self.selection_bar(ui, page, inventory, usage, blocked, helper, action);
        });
        let table_height = body_height(ui);
        let sort = (page.sort, page.descending);
        let mut sort_clicked = None;
        let running = page.run.as_ref().filter(|r| !r.finished);
        let mut toggled: Option<u64> = None;
        ui.push_id("installed", |ui| {
            TableBuilder::new(ui)
                .striped(true)
                .sense(Sense::click())
                .cell_layout(Layout::left_to_right(Align::Center))
                .column(Column::exact(22.0))
                .column(Column::remainder().at_least(160.0).clip(true))
                .column(Column::exact(72.0))
                .column(Column::exact(130.0))
                .column(Column::exact(64.0))
                .column(Column::exact(150.0))
                .min_scrolled_height(table_height)
                .max_scroll_height(table_height)
                .header(HEADER_HEIGHT, |mut header| {
                    header.col(|_| {});
                    for (label, by) in [
                        ("Mod", Sort::Name),
                        ("Size", Sort::Size),
                        ("Updated", Sort::Updated),
                        ("Used by", Sort::UsedBy),
                    ] {
                        header.col(|ui| {
                            let mut response = sort_header(ui, label, by, sort);
                            if by == Sort::UsedBy {
                                response = response.on_hover_text(format!(
                                    "How many of your servers list this mod.\n\n{YOUR_SERVERS}"
                                ));
                            }
                            if response.clicked() {
                                sort_clicked = Some(by);
                            }
                        });
                    }
                    header.col(|ui| {
                        ui.strong("");
                    });
                })
                .body(|body| {
                    body.rows(24.0, rows.len(), |mut row| {
                        let r = &rows[row.index()];
                        let id = r.folder.id;
                        let gone = page.unsubscribed.contains(&id);
                        let progress = page.run.as_ref().and_then(|run| run.items.get(&id));
                        row.col(|ui| {
                            let mut checked = page.selected.contains(&id);
                            if ui
                                .add_enabled(!gone, egui::Checkbox::without_text(&mut checked))
                                .changed()
                            {
                                toggled = Some(id);
                            }
                        });
                        row.col(|ui| {
                            let label = ui.add(egui::Label::new(&r.name).truncate());
                            let mut hover = format!("Workshop id {id}");
                            if let Some(title) = r.title {
                                hover = format!("{title}\n{hover}");
                            }
                            label.on_hover_text(hover);
                            if r.folder.other_account.is_some() {
                                ui.label(RichText::new("other account").weak().small())
                                    .on_hover_text(
                                        "Steam recorded this mod under another Steam account on \
                                         this computer. It keeps one account per mod, so yours \
                                         may use it too.",
                                    );
                            }
                        });
                        row.col(|ui| {
                            ui.label(size_text(r.folder.size));
                        });
                        row.col(|ui| {
                            let text = age_text(r.folder.time_updated);
                            let label = ui.label(text);
                            if r.folder.time_updated > 0 {
                                label.on_hover_text(date_text(r.folder.time_updated));
                            }
                            if r.outdated {
                                ui.label(RichText::new("⟳ update").color(FAIR))
                                    .on_hover_text("The Workshop has a newer version");
                            }
                        });
                        row.col(|ui| {
                            if r.users.is_empty() {
                                ui.label(RichText::new("none").weak())
                                    .on_hover_text(format!(
                                        "None of your servers lists it.\n\n{YOUR_SERVERS}"
                                    ));
                            } else {
                                let names: Vec<&str> = r
                                    .users
                                    .iter()
                                    .map(|&i| usage.servers[i].0.as_str())
                                    .collect();
                                ui.add(
                                    egui::Label::new(format!(
                                        "{} server{}",
                                        names.len(),
                                        plural(names.len())
                                    ))
                                    .sense(Sense::hover()),
                                )
                                .on_hover_text(names_list(&names));
                            }
                        });
                        let unsubscribing = running.is_some()
                            && matches!(progress, Some((ItemState::Unsubscribing, _)));
                        // Why unsubscribing can't be done now, if it can't.
                        let why_not = if gone {
                            Some("Already unsubscribed")
                        } else if unsubscribing {
                            Some("Unsubscribing…")
                        } else if !helper {
                            Some(
                                "The Workshop helper program is missing; unsubscribe on the \
                                 mod's Workshop page instead",
                            )
                        } else {
                            blocked
                        };
                        let mut menu = |ui: &mut egui::Ui| row_menu(ui, r.folder, why_not, action);
                        row.col(|ui| {
                            ui.menu_button("…", &mut menu).response.on_hover_text(
                                "Its Workshop page, its folder, unsubscribing (or right-click \
                                 the row)",
                            );
                            match progress {
                                _ if unsubscribing => {
                                    ui.spinner();
                                    ui.label(RichText::new("unsubscribing…").weak());
                                }
                                Some((ItemState::Failed, message)) if !gone => {
                                    let failed = ui.label(RichText::new("failed").color(ERROR));
                                    if let Some(m) = message {
                                        failed.on_hover_text(m);
                                    }
                                }
                                _ if gone => {
                                    ui.label(RichText::new("unsubscribed").color(GOOD))
                                        .on_hover_text(
                                            "Steam deletes the files once it doesn't count DayZ \
                                             as running any more",
                                        );
                                }
                                _ => {}
                            }
                        });
                        egui::Popup::context_menu(&row.response())
                            .id(egui::Id::new(("mods row menu", id)))
                            .show(menu);
                    });
                });
        });
        if let Some(id) = toggled
            && !page.selected.remove(&id)
        {
            page.selected.insert(id);
        }
        if let Some(by) = sort_clicked {
            if page.sort == by {
                page.descending = !page.descending;
            } else {
                page.sort = by;
                page.descending = by != Sort::Name;
            }
        }
    }

    /// The installed mods to show, searched and sorted.
    fn installed_rows<'a>(
        &'a self,
        page: &ModsPage,
        inventory: &'a Inventory,
        usage: &'a Usage,
    ) -> Vec<Row<'a>> {
        // Names the servers gave, for mods without a name of their own.
        let mut server_names: HashMap<u64, &str> = HashMap::new();
        for (_, mods) in &usage.servers {
            for m in mods.iter().flatten() {
                if !m.name.is_empty() {
                    server_names.entry(m.id).or_insert(&m.name);
                }
            }
        }
        let query = page.search.trim().to_lowercase();
        let mut rows: Vec<Row> = inventory
            .installed
            .iter()
            .filter_map(|folder| {
                let id = folder.id;
                let details = self.details.get(&id);
                let title = details.map(|d| d.title.as_str()).filter(|t| !t.is_empty());
                let name = shown_name(folder, title, server_names.get(&id).copied());
                if !query.is_empty()
                    && !name.to_lowercase().contains(&query)
                    && !title.is_some_and(|t| t.to_lowercase().contains(&query))
                    && !id.to_string().contains(&query)
                {
                    return None;
                }
                let outdated =
                    self.local.state(id, details.map(|d| d.time_updated)) == ModState::Outdated;
                Some(Row {
                    folder,
                    title: title.filter(|t| *t != name),
                    name,
                    outdated,
                    users: usage.users(id),
                })
            })
            .collect();
        rows.sort_by(|a, b| {
            let order = match page.sort {
                Sort::Name => std::cmp::Ordering::Equal,
                Sort::Size => a.folder.size.cmp(&b.folder.size),
                Sort::Updated => a.folder.time_updated.cmp(&b.folder.time_updated),
                Sort::UsedBy => a.users.len().cmp(&b.users.len()),
            };
            let order = order.then_with(|| {
                a.name
                    .to_lowercase()
                    .cmp(&b.name.to_lowercase())
                    .then(a.folder.id.cmp(&b.folder.id))
            });
            if page.descending {
                order.reverse()
            } else {
                order
            }
        });
        rows
    }

    /// Below the installed mods: selecting, unsubscribing from the selection (after asking), and
    /// what happened.
    #[allow(clippy::too_many_arguments)]
    fn selection_bar(
        &self,
        ui: &mut egui::Ui,
        page: &mut ModsPage,
        inventory: &Inventory,
        usage: &Usage,
        blocked: Option<&'static str>,
        helper: bool,
        action: &mut Option<Action>,
    ) {
        ui.horizontal(|ui| {
            let unused: Vec<u64> = inventory
                .installed
                .iter()
                .map(|f| f.id)
                .filter(|id| usage.users(*id).is_empty() && !page.unsubscribed.contains(id))
                .collect();
            let select = ui
                .add_enabled(
                    !unused.is_empty(),
                    egui::Button::new(format!("Select unused mods ({})", unused.len())),
                )
                .on_hover_text(format!(
                    "Selects the mods none of your servers lists.\n\n{YOUR_SERVERS}"
                ));
            if select.clicked() {
                page.selected = unused.into_iter().collect();
            }
            if !page.selected.is_empty() && ui.button("Clear selection").clicked() {
                page.selected.clear();
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let size: u64 = page
                    .selected
                    .iter()
                    .filter_map(|id| inventory.get(*id))
                    .map(|f| f.size)
                    .sum();
                let reason = if page.selected.is_empty() {
                    Some("Select some mods first")
                } else if !helper {
                    Some("The Workshop helper program is missing")
                } else {
                    blocked
                };
                let n = page.selected.len();
                let label = if n == 0 {
                    "Unsubscribe selected…".to_string()
                } else {
                    format!(
                        "Unsubscribe selected ({n} mod{}, {})…",
                        plural(n),
                        size_text(size)
                    )
                };
                let unsubscribe = ui
                    .add_enabled(reason.is_none(), egui::Button::new(label))
                    .on_disabled_hover_text(reason.unwrap_or_default());
                if unsubscribe.clicked() {
                    *action = Some(Action::Ask(Confirm::Unsubscribe(
                        page.selected.iter().copied().collect(),
                    )));
                }
            });
        });
        status_line(ui, page);
    }

    /// Asks, in a small window over the Mods window, before unsubscribing or deleting a folder.
    fn confirm_modal(
        &self,
        ctx: &egui::Context,
        page: &ModsPage,
        usage: &Usage,
        blocked: Option<&'static str>,
        action: &mut Option<Action>,
    ) {
        let (Some(confirm), Some(inventory)) = (&page.confirm, &page.inventory) else {
            return;
        };
        let modal = egui::Modal::new(egui::Id::new("mods-confirm")).show(ctx, |ui| {
            ui.set_max_width(420.0);
            match confirm {
                Confirm::Unsubscribe(ids) => {
                    self.unsubscribe_confirm(ui, ids, inventory, usage, blocked, action)
                }
                Confirm::Delete(id) => delete_confirm(ui, *id, inventory, blocked, action),
            }
        });
        if modal.should_close() && action.is_none() {
            *action = Some(Action::Cancel);
        }
    }

    fn unsubscribe_confirm(
        &self,
        ui: &mut egui::Ui,
        ids: &[u64],
        inventory: &Inventory,
        usage: &Usage,
        blocked: Option<&'static str>,
        action: &mut Option<Action>,
    ) {
        let folders: Vec<&Folder> = ids.iter().filter_map(|id| inventory.get(*id)).collect();
        let size: u64 = folders.iter().map(|f| f.size).sum();
        let names: Vec<String> = folders.iter().map(|f| self.mod_name(f, usage)).collect();
        let used = ids
            .iter()
            .filter(|id| !usage.users(**id).is_empty())
            .count();
        let question = match names.as_slice() {
            [name] => format!("Unsubscribe from {name} ({})?", size_text(size)),
            _ => format!("Unsubscribe from {} mods ({})?", ids.len(), size_text(size)),
        };
        let heading = ui.label(RichText::new(question).strong());
        if names.len() > 1 {
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            heading.on_hover_text(names_list(&names));
        }
        let (files, them) = if ids.len() == 1 {
            ("its files", "it")
        } else {
            ("their files", "them")
        };
        ui.label(format!(
            "Steam deletes {files} once it doesn't count DayZ as running any more: a moment after \
             the browser's helper is done. A server that needs {them} later downloads {them} \
             again."
        ));
        if used > 0 {
            let text = match (ids.len(), used) {
                (1, _) => "One of your servers uses it.".to_string(),
                (_, 1) => "1 of them is used by your servers.".into(),
                (_, n) => format!("{n} of them are used by your servers."),
            };
            ui.label(RichText::new(text).color(WARN))
                .on_hover_text(YOUR_SERVERS);
        }
        let unknown = usage.unknown();
        if !unknown.is_empty() {
            let (servers, they) = if unknown.len() == 1 {
                ("1 of your servers".to_string(), "it uses")
            } else {
                (format!("{} of your servers", unknown.len()), "they use")
            };
            let needed = if ids.len() == 1 {
                "this may still be needed"
            } else {
                "some of these may still be needed"
            };
            ui.label(
                RichText::new(format!(
                    "{servers} didn't say which mods {they}, so {needed}."
                ))
                .color(WARN),
            )
            .on_hover_text(names_list(&unknown));
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let go = ui
                .add_enabled(blocked.is_none(), egui::Button::new("Unsubscribe"))
                .on_disabled_hover_text(blocked.unwrap_or_default());
            if go.clicked() {
                *action = Some(Action::Unsubscribe(ids.to_vec()));
            }
            if ui.button("Cancel").clicked() {
                *action = Some(Action::Cancel);
            }
        });
    }

    /// The name a mod is shown by: its own (from `meta.cpp`), the Workshop's, or a server's.
    fn mod_name(&self, folder: &Folder, usage: &Usage) -> String {
        let title = self
            .details
            .get(&folder.id)
            .map(|d| d.title.as_str())
            .filter(|t| !t.is_empty());
        let from_server = || {
            usage
                .servers
                .iter()
                .flat_map(|(_, mods)| mods.iter().flatten())
                .find(|m| m.id == folder.id && !m.name.is_empty())
                .map(|m| m.name.as_str())
        };
        shown_name(folder, title, from_server())
    }

    fn mods_action(&mut self, page: &mut ModsPage, action: Action) {
        match action {
            Action::OpenPage(id) => open_workshop(id),
            Action::OpenFolder(path) => {
                if let Err(e) = open::that_detached(&path) {
                    page.message = Some((format!("Couldn't open {}: {e}", path.display()), true));
                }
            }
            Action::Ask(confirm) => {
                page.message = None;
                page.confirm = Some(confirm);
            }
            Action::Cancel => page.confirm = None,
            Action::Rescan => {
                page.message = None;
                self.start_scan(page);
            }
            Action::Unsubscribe(ids) => {
                page.confirm = None;
                // Checked again: DayZ may have started since the button was drawn.
                if let Some(reason) = self.mods_blocked(page) {
                    page.message = Some((reason.into(), true));
                    return;
                }
                let run = join::NEXT_RUN.fetch_add(1, Ordering::Relaxed);
                let tx = self.tx.clone();
                let ctx = self.ctx.clone();
                let started = workshop::Helper::unsubscribe(&ids, move |event| {
                    let _ = tx.send(Msg::Workshop(run, event));
                    ctx.request_repaint();
                });
                match started {
                    Ok(helper) => {
                        page.message = None;
                        page.selected.retain(|id| !ids.contains(id));
                        page.run = Some(Run {
                            id: run,
                            _helper: helper,
                            connected: false,
                            items: ids
                                .iter()
                                .map(|&id| (id, (ItemState::Unsubscribing, None)))
                                .collect(),
                            error: None,
                            finished: false,
                        });
                    }
                    Err(e) => page.message = Some((format!("{e:#}"), true)),
                }
            }
            Action::Delete(id) => {
                page.confirm = None;
                if let Some(reason) = self.mods_blocked(page) {
                    page.message = Some((reason.into(), true));
                    return;
                }
                let Ok(install) = &self.install else { return };
                let install = install.clone();
                page.deleting = Some(id);
                page.message = None;
                self.spawn(move || {
                    let result =
                        inventory::delete_leftover(&install, id).map_err(|e| format!("{e:#}"));
                    Msg::Mods(PageMsg::Deleted(id, result))
                });
            }
        }
    }
}

/// The leftover folders, with deleting (after asking).
fn leftovers_tab(
    ui: &mut egui::Ui,
    page: &mut ModsPage,
    inventory: &Inventory,
    blocked: Option<&'static str>,
    action: &mut Option<Action>,
) {
    ui.label(
        "Folders in DayZ's Workshop folder that Steam didn't install: left behind by Steam, or \
         another account's on this computer that Steam hasn't finished getting. Deleting one only \
         frees the space; the join window downloads a mod again when a server needs it.",
    );
    if inventory.no_records {
        ui.label(
            RichText::new(
                "Steam's record of installed mods (appworkshop_221100.acf) couldn't be read, so \
                 no folder is listed here.",
            )
            .color(WARN),
        );
    } else if inventory.account.is_none() {
        ui.label(
            RichText::new(
                "Your Steam account couldn't be told from Steam's files, so other accounts' mods \
                 count as installed.",
            )
            .weak(),
        );
    }
    ui.add_space(4.0);
    if inventory.leftovers.is_empty() {
        ui.label(RichText::new("No leftover folders.").weak());
        status_line(ui, page);
        return;
    }
    bottom_panel("mods-leftovers-bottom").show(ui, |ui| {
        status_line(ui, page);
    });
    let mut folders: Vec<&Folder> = inventory.leftovers.iter().collect();
    folders.sort_by(|a, b| b.size.cmp(&a.size).then(a.id.cmp(&b.id)));
    let table_height = body_height(ui);
    ui.push_id("leftovers", |ui| {
        TableBuilder::new(ui)
            .striped(true)
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::remainder().at_least(160.0).clip(true))
            .column(Column::exact(72.0))
            .column(Column::exact(250.0))
            .column(Column::exact(190.0))
            .min_scrolled_height(table_height)
            .max_scroll_height(table_height)
            .header(HEADER_HEIGHT, |mut header| {
                for label in ["Folder", "Size", "Why it's left over", ""] {
                    header.col(|ui| {
                        ui.strong(label);
                    });
                }
            })
            .body(|body| {
                body.rows(24.0, folders.len(), |mut row| {
                    let f = folders[row.index()];
                    row.col(|ui| {
                        let name = match &f.name {
                            Some(name) => format!("{name} ({})", f.id),
                            None => f.id.to_string(),
                        };
                        ui.add(egui::Label::new(name).truncate())
                            .on_hover_text(f.path.display().to_string());
                    });
                    row.col(|ui| {
                        ui.label(size_text(f.size));
                    });
                    row.col(|ui| {
                        let why = match f.leftover {
                            _ if f.link => "A link to a folder elsewhere",
                            Some(Leftover::OtherAccount(_)) => {
                                "Another Steam account's, not installed"
                            }
                            _ => "Steam has no record of it",
                        };
                        ui.label(why);
                    });
                    row.col(|ui| {
                        if ui.small_button("Folder").clicked() {
                            *action = Some(Action::OpenFolder(f.path.clone()));
                        }
                        if page.deleting == Some(f.id) {
                            ui.spinner();
                            ui.label(RichText::new("deleting…").weak());
                            return;
                        }
                        let reason = if f.link {
                            Some("It's a link to a folder elsewhere; not deleting through it")
                        } else {
                            blocked
                        };
                        let delete = ui
                            .add_enabled(
                                reason.is_none(),
                                egui::Button::new("Delete folder…").small(),
                            )
                            .on_disabled_hover_text(reason.unwrap_or_default());
                        if delete.clicked() {
                            *action = Some(Action::Ask(Confirm::Delete(f.id)));
                        }
                    });
                });
            });
    });
}

/// Asks before deleting a leftover folder.
fn delete_confirm(
    ui: &mut egui::Ui,
    id: u64,
    inventory: &Inventory,
    blocked: Option<&'static str>,
    action: &mut Option<Action>,
) {
    let Some(f) = inventory.get(id) else {
        // Gone since: nothing to ask.
        *action = Some(Action::Cancel);
        return;
    };
    let name = match &f.name {
        Some(name) => format!("{name} ({id})"),
        None => format!("folder {id}"),
    };
    ui.label(
        RichText::new(format!(
            "Delete {name} ({})? This can't be undone.",
            size_text(f.size)
        ))
        .strong(),
    );
    ui.label(RichText::new(f.path.display().to_string()).weak().small());
    if matches!(f.leftover, Some(Leftover::OtherAccount(_))) {
        ui.label(
            RichText::new(
                "Another Steam account on this computer subscribed to it; Steam may download it \
                 again when that account signs in.",
            )
            .color(WARN),
        );
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let go = ui
            .add_enabled(blocked.is_none(), egui::Button::new("Delete"))
            .on_disabled_hover_text(blocked.unwrap_or_default());
        if go.clicked() {
            *action = Some(Action::Delete(id));
        }
        if ui.button("Cancel").clicked() {
            *action = Some(Action::Cancel);
        }
    });
}

/// The helper's progress, and the last result or error.
fn status_line(ui: &mut egui::Ui, page: &ModsPage) {
    if let Some(run) = page.run.as_ref().filter(|r| !r.finished) {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(if run.connected {
                format!(
                    "Unsubscribing from {} mod{} through Steam…",
                    run.items.len(),
                    plural(run.items.len())
                )
            } else {
                "Reaching Steam… (Steam shows DayZ as running meanwhile)".into()
            });
        });
    }
    if let Some((message, error)) = &page.message {
        let color = if *error {
            ERROR
        } else {
            ui.visuals().text_color()
        };
        ui.label(RichText::new(message).color(color));
    }
}

/// A panel along the bottom of the window, laid out before the table above it.
fn bottom_panel(id: &'static str) -> egui::Panel {
    egui::Panel::bottom(id)
        .resizable(false)
        .show_separator_line(false)
        .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(0, 4)))
}

/// The height left for a table's rows, below its header.
fn body_height(ui: &egui::Ui) -> f32 {
    (ui.available_height() - HEADER_HEIGHT - 2.0 * ui.spacing().item_spacing.y).max(80.0)
}

/// The tables' header row.
const HEADER_HEIGHT: f32 = 22.0;

/// The name a mod is shown by: its own (from `meta.cpp`), else the Workshop's `title`, else
/// the one a server gave, else its id.
fn shown_name(folder: &Folder, title: Option<&str>, from_server: Option<&str>) -> String {
    folder
        .name
        .clone()
        .or_else(|| title.map(String::from))
        .or_else(|| from_server.map(String::from))
        .unwrap_or_else(|| format!("Workshop item {}", folder.id))
}

/// A sortable column's header, which sorts by it when clicked.
fn sort_header(
    ui: &mut egui::Ui,
    label: &str,
    by: Sort,
    (sort, descending): (Sort, bool),
) -> egui::Response {
    let arrow = match (sort == by, descending) {
        (false, _) => "",
        (true, true) => " ⏷",
        (true, false) => " ⏶",
    };
    ui.add(
        egui::Label::new(RichText::new(format!("{label}{arrow}")).strong()).sense(Sense::click()),
    )
}

/// The menu of an installed mod's row, from its … button or a right-click; `why_not` says why it
/// can't be unsubscribed from now, if it can't. Each item does its thing and closes the menu.
fn row_menu(
    ui: &mut egui::Ui,
    folder: &Folder,
    why_not: Option<&str>,
    action: &mut Option<Action>,
) {
    if ui
        .button("Workshop page")
        .on_hover_text("Open its Workshop page in Steam")
        .clicked()
    {
        *action = Some(Action::OpenPage(folder.id));
        ui.close();
    }
    if ui
        .button("Open folder")
        .on_hover_text(folder.path.display().to_string())
        .clicked()
    {
        *action = Some(Action::OpenFolder(folder.path.clone()));
        ui.close();
    }
    ui.separator();
    let unsubscribe = ui
        .add_enabled(why_not.is_none(), egui::Button::new("Unsubscribe…"))
        .on_disabled_hover_text(why_not.unwrap_or_default());
    if unsubscribe.clicked() {
        *action = Some(Action::Ask(Confirm::Unsubscribe(vec![folder.id])));
        ui.close();
    }
}

/// What an unsubscribing run did, and whether that's an error: how many mods Steam unsubscribed,
/// refused, and didn't answer for before the helper quit.
fn run_summary(done: usize, failed: usize, unanswered: usize) -> (String, bool) {
    if done + failed + unanswered == 0 {
        return (
            "The Workshop helper finished without saying what it did".into(),
            true,
        );
    }
    let mut parts = Vec::new();
    if done > 0 {
        parts.push(format!("Unsubscribed from {done} mod{}", plural(done)));
    }
    if failed > 0 {
        parts.push(format!(
            "Steam couldn't unsubscribe from {failed} (hover over “failed” to see why)"
        ));
    }
    if unanswered > 0 {
        parts.push(format!(
            "the helper stopped before Steam answered for {unanswered}"
        ));
    }
    if failed == 0 && unanswered == 0 {
        let files = if done == 1 {
            "its files"
        } else {
            "their files"
        };
        return (
            format!("{}. Steam deletes {files} in a moment.", parts.join("")),
            false,
        );
    }
    let mut text = parts.join("; ");
    if let Some(first) = text.get(..1) {
        text = first.to_uppercase() + &text[1..];
    }
    (text, true)
}

/// Up to a dozen names, one per line.
fn names_list(names: &[&str]) -> String {
    let mut text: Vec<String> = names.iter().take(12).map(|n| n.to_string()).collect();
    if names.len() > 12 {
        text.push(format!("and {} more", names.len() - 12));
    }
    text.join("\n")
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Bytes as GB, MB, or KB (powers of 1,000, as Steam shows them).
pub(super) fn size_text(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.0} MB", b / 1e6)
    } else if bytes == 0 {
        "0 KB".into()
    } else {
        format!("{} KB", bytes.div_ceil(1000))
    }
}

/// How long ago a Unix time was, roughly.
fn age_text(time: u64) -> String {
    if time == 0 {
        return "unknown".into();
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    age_between(time, now)
}

fn age_between(time: u64, now: u64) -> String {
    let days = now.saturating_sub(time) / 86_400;
    match days {
        0 => "today".into(),
        1 => "yesterday".into(),
        2..=59 => format!("{days} days ago"),
        60..=729 => format!("{} months ago", days / 30),
        _ => format!("{} years ago", days / 365),
    }
}

/// A Unix time's date (UTC), as `2026-04-11`.
fn date_text(time: u64) -> String {
    // Howard Hinnant's days-to-civil algorithm.
    let z = (time / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_times_read_well() {
        assert_eq!(size_text(0), "0 KB");
        assert_eq!(size_text(2649), "3 KB");
        assert_eq!(size_text(527_656), "528 KB");
        assert_eq!(size_text(55_377_498), "55 MB");
        assert_eq!(size_text(11_657_993_531), "11.7 GB");
        assert_eq!(date_text(0), "1970-01-01");
        assert_eq!(date_text(1_771_519_119), "2026-02-19");
        assert_eq!(date_text(951_782_400), "2000-02-29");
        let day = 86_400;
        assert_eq!(age_between(10 * day, 10 * day + 5), "today");
        assert_eq!(age_between(10 * day, 11 * day), "yesterday");
        assert_eq!(age_between(0, 45 * day), "45 days ago");
        assert_eq!(age_between(0, 200 * day), "6 months ago");
        assert_eq!(age_between(0, 800 * day), "2 years ago");
        assert_eq!(age_between(5 * day, 0), "today", "a time in the future");
    }

    #[test]
    fn sums_up_unsubscribing() {
        assert_eq!(
            run_summary(1, 0, 0),
            (
                "Unsubscribed from 1 mod. Steam deletes its files in a moment.".into(),
                false
            )
        );
        assert_eq!(
            run_summary(3, 0, 0).0,
            "Unsubscribed from 3 mods. Steam deletes their files in a moment."
        );
        let (text, error) = run_summary(0, 1, 0);
        assert!(error);
        assert!(text.starts_with("Steam couldn't unsubscribe from 1"));
        let (text, _) = run_summary(2, 1, 1);
        assert!(text.starts_with("Unsubscribed from 2 mods; Steam couldn't"));
        assert!(text.ends_with("the helper stopped before Steam answered for 1"));
        assert_eq!(
            run_summary(0, 0, 2).0,
            "The helper stopped before Steam answered for 2"
        );
        assert!(run_summary(0, 0, 0).1);
    }
}
