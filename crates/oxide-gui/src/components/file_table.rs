//! The archive browser: a folder tree derived from the flat entry list, and
//! the sortable table that shows one folder (or search results) at a time.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use eframe::egui::{self, Align, Layout, RichText, Sense};
use egui_extras::{Column, TableBuilder};
use oxide_core::{Entry, EntryKind};

use super::icons::{self, Icon};
use super::{format_time, human_bytes};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SortKey {
    Name,
    Size,
    Kind,
    Modified,
}

/// One line in the browser.
#[derive(Debug, Clone)]
pub struct Row {
    pub name: String,
    /// Archive path without a trailing slash; used for selection and navigation.
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<i64>,
    pub kind: String,
    pub encrypted: bool,
    pub link: Option<String>,
}

/// Folder → children index built once per opened archive.
#[derive(Default)]
pub struct Tree {
    children: HashMap<String, Vec<Row>>,
    all: Vec<Row>,
}

fn kind_label(e: &Entry, name: &str) -> String {
    match e.kind {
        EntryKind::Directory => "Folder".into(),
        EntryKind::Symlink | EntryKind::Hardlink => "Link".into(),
        EntryKind::Other => "Special".into(),
        EntryKind::File => match name.rsplit_once('.') {
            Some((stem, ext)) if !stem.is_empty() && ext.len() <= 6 => format!("{} file", ext.to_uppercase()),
            _ => "File".into(),
        },
    }
}

fn normalize(path: &str) -> String {
    path.split('/').filter(|p| !p.is_empty() && *p != ".").collect::<Vec<_>>().join("/")
}

impl Tree {
    pub fn build(entries: &[Entry]) -> Tree {
        let mut children: HashMap<String, Vec<Row>> = HashMap::new();
        let mut dirs: HashSet<String> = HashSet::new();
        let mut dir_sizes: HashMap<String, u64> = HashMap::new();
        let mut dir_times: HashMap<String, i64> = HashMap::new();
        let mut all = Vec::with_capacity(entries.len());
        children.entry(String::new()).or_default();

        for e in entries {
            let path = normalize(&e.path);
            if path.is_empty() {
                continue;
            }
            let parts: Vec<&str> = path.split('/').collect();
            let mut parent = String::new();
            for (i, part) in parts.iter().enumerate() {
                let full = if parent.is_empty() { (*part).to_owned() } else { format!("{parent}/{part}") };
                let last = i + 1 == parts.len();
                if !last || e.is_dir() {
                    if dirs.insert(full.clone()) {
                        children.entry(full.clone()).or_default();
                        children.entry(parent.clone()).or_default().push(Row {
                            name: (*part).to_owned(),
                            path: full.clone(),
                            is_dir: true,
                            size: 0,
                            modified: None,
                            kind: "Folder".into(),
                            encrypted: false,
                            link: None,
                        });
                    }
                    if last && let Some(t) = e.modified {
                        dir_times.insert(full.clone(), t);
                    }
                    if !last {
                        *dir_sizes.entry(full.clone()).or_default() += e.size.unwrap_or(0);
                    }
                } else {
                    let row = Row {
                        name: (*part).to_owned(),
                        path: full.clone(),
                        is_dir: false,
                        size: e.size.unwrap_or(0),
                        modified: e.modified,
                        kind: kind_label(e, part),
                        encrypted: e.encrypted,
                        link: e.link_target.clone(),
                    };
                    all.push(row.clone());
                    children.entry(parent.clone()).or_default().push(row);
                }
                parent = full;
            }
        }
        for rows in children.values_mut() {
            for row in rows.iter_mut().filter(|r| r.is_dir) {
                row.size = dir_sizes.get(&row.path).copied().unwrap_or(0);
                row.modified = dir_times.get(&row.path).copied();
            }
        }
        let mut dir_rows: Vec<Row> = children.values().flatten().filter(|r| r.is_dir).cloned().collect();
        all.append(&mut dir_rows);
        Tree { children, all }
    }

    /// Every folder, including ones only implied by file paths.
    pub fn dir_count(&self) -> usize {
        self.children.len().saturating_sub(1) // minus the root
    }

    pub fn children(&self, dir: &str) -> &[Row] {
        self.children.get(dir).map(Vec::as_slice).unwrap_or(&[])
    }

    /// If the root holds exactly one folder and nothing else, its path.
    pub fn single_root_dir(&self) -> Option<String> {
        match self.children("") {
            [only] if only.is_dir => Some(only.path.clone()),
            _ => None,
        }
    }

    /// Case-insensitive substring search over full paths.
    pub fn search(&self, query: &str) -> Vec<Row> {
        let q = query.to_lowercase();
        self.all
            .iter()
            .filter(|r| r.path.to_lowercase().contains(&q))
            .map(|r| Row { name: r.path.clone(), ..r.clone() })
            .collect()
    }
}

pub fn sort_rows(rows: &mut [Row], key: SortKey, ascending: bool) {
    rows.sort_by(|a, b| {
        // Folders always first, regardless of direction.
        let dirs = b.is_dir.cmp(&a.is_dir);
        if dirs != Ordering::Equal {
            return dirs;
        }
        let ord = match key {
            SortKey::Name => natural_cmp(&a.name, &b.name),
            SortKey::Size => a.size.cmp(&b.size),
            SortKey::Kind => a.kind.cmp(&b.kind).then_with(|| natural_cmp(&a.name, &b.name)),
            SortKey::Modified => a.modified.cmp(&b.modified),
        };
        if ascending { ord } else { ord.reverse() }
    });
}

/// Case-insensitive comparison that orders embedded numbers numerically
/// ("file2" before "file10").
fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let take = |it: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut s = String::new();
                    while let Some(c) = it.peek().filter(|c| c.is_ascii_digit()) {
                        s.push(*c);
                        it.next();
                    }
                    s
                };
                let (na, nb) = (take(&mut a), take(&mut b));
                let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let ord = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(x), Some(y)) => {
                let ord = x.to_lowercase().cmp(y.to_lowercase());
                if ord != Ordering::Equal {
                    return ord;
                }
                a.next();
                b.next();
            }
        }
    }
}

/// What the user did in the table this frame.
#[derive(Default)]
pub struct TableEvents {
    pub open_dir: Option<String>,
    pub sort: Option<SortKey>,
    pub extract_selection: bool,
}

/// Draws the table and updates `selection` in place.
pub fn show(
    ui: &mut egui::Ui,
    rows: &[Row],
    selection: &mut HashSet<String>,
    anchor: &mut Option<usize>,
    sort: SortKey,
    ascending: bool,
    scroll_to: Option<usize>,
) -> TableEvents {
    let mut events = TableEvents::default();
    let weak = crate::theme::weak(ui);
    let modifiers = ui.input(|i| i.modifiers);
    let arrow = if ascending { " ⏶" } else { " ⏷" };

    let folder_color = crate::theme::ACCENT.gamma_multiply(0.85);
    let mut table = TableBuilder::new(ui);
    if let Some(i) = scroll_to {
        table = table.scroll_to_row(i, None);
    }
    table
        .striped(true)
        .sense(Sense::click())
        .cell_layout(Layout::left_to_right(Align::Center))
        .column(Column::remainder().at_least(180.0).clip(true))
        .column(Column::initial(90.0).at_least(60.0))
        .column(Column::initial(110.0).at_least(60.0).clip(true))
        .column(Column::initial(140.0).at_least(80.0))
        .auto_shrink([false, false])
        .header(26.0, |mut header| {
            for (key, label) in [
                (SortKey::Name, "Name"),
                (SortKey::Size, "Size"),
                (SortKey::Kind, "Type"),
                (SortKey::Modified, "Modified"),
            ] {
                header.col(|ui| {
                    let text = if sort == key { format!("{label}{arrow}") } else { label.to_owned() };
                    let button = egui::Button::new(RichText::new(text).strong()).frame(false);
                    // Sizes are right-aligned, so their heading is too.
                    let clicked = if key == SortKey::Size {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.add(button)).inner.clicked()
                    } else {
                        ui.add(button).clicked()
                    };
                    if clicked {
                        events.sort = Some(key);
                    }
                });
            }
        })
        .body(|body| {
            body.rows(26.0, rows.len(), |mut row| {
                let i = row.index();
                let r = &rows[i];
                row.set_selected(selection.contains(&r.path));
                row.col(|ui| {
                    let (icon, color) = if r.is_dir {
                        (Icon::Folder, folder_color)
                    } else if r.link.is_some() {
                        (Icon::Link, weak)
                    } else {
                        (Icon::File, weak)
                    };
                    icons::show(ui, icon, color);
                    // Lock pinned to the right; the name truncates in the remaining space.
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if r.encrypted {
                            icons::show(ui, Icon::Lock, weak).on_hover_text("Encrypted");
                        }
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            let mut name = RichText::new(&r.name);
                            if r.is_dir {
                                name = name.strong();
                            }
                            let label = ui.add(egui::Label::new(name).selectable(false).truncate());
                            if let Some(target) = &r.link {
                                label.on_hover_text(format!("Link to {target}"));
                            }
                        });
                    });
                });
                row.col(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let text = if r.is_dir && r.size == 0 { String::new() } else { human_bytes(r.size) };
                        ui.label(RichText::new(text).color(weak));
                    });
                });
                row.col(|ui| {
                    ui.add(egui::Label::new(RichText::new(&r.kind).color(weak)).selectable(false).truncate());
                });
                row.col(|ui| {
                    ui.label(RichText::new(format_time(r.modified)).color(weak));
                });

                let response = row.response();
                if response.clicked() {
                    if modifiers.command {
                        if !selection.remove(&r.path) {
                            selection.insert(r.path.clone());
                        }
                        *anchor = Some(i);
                    } else if modifiers.shift
                        && let Some(a) = *anchor
                    {
                        selection.clear();
                        let (lo, hi) = (a.min(i), a.max(i));
                        selection.extend(rows[lo..=hi].iter().map(|r| r.path.clone()));
                    } else {
                        selection.clear();
                        selection.insert(r.path.clone());
                        *anchor = Some(i);
                    }
                }
                if response.double_clicked() && r.is_dir {
                    events.open_dir = Some(r.path.clone());
                }
                response.context_menu(|ui| {
                    if !selection.contains(&r.path) {
                        selection.clear();
                        selection.insert(r.path.clone());
                    }
                    if r.is_dir && ui.button("Open folder").clicked() {
                        events.open_dir = Some(r.path.clone());
                        ui.close();
                    }
                    let label = format!("Extract {} selected…", selection.len());
                    if ui.button(label).clicked() {
                        events.extract_selection = true;
                        ui.close();
                    }
                    if ui.button("Copy path").clicked() {
                        ui.ctx().copy_text(r.path.clone());
                        ui.close();
                    }
                });
            });
        });
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, kind: EntryKind, size: u64) -> Entry {
        let mut e: Entry = serde_like(path, kind);
        e.size = Some(size);
        e
    }

    // `Entry::new` is crate-private in oxide-core; build entries via a listing-shaped value.
    fn serde_like(path: &str, kind: EntryKind) -> Entry {
        Entry {
            path: path.into(),
            kind,
            size: None,
            compressed_size: None,
            modified: None,
            mode: None,
            encrypted: false,
            link_target: None,
        }
    }

    #[test]
    fn builds_implicit_folders_and_sizes() {
        let tree = Tree::build(&[
            entry("a/b/c.txt", EntryKind::File, 10),
            entry("a/d.txt", EntryKind::File, 5),
            entry("top.txt", EntryKind::File, 1),
            entry("a/", EntryKind::Directory, 0),
        ]);
        let root = tree.children("");
        assert_eq!(root.len(), 2);
        let a = root.iter().find(|r| r.name == "a").unwrap();
        assert!(a.is_dir);
        assert_eq!(a.size, 15);
        assert_eq!(tree.children("a").len(), 2);
        assert_eq!(tree.children("a/b")[0].name, "c.txt");
        assert_eq!(tree.search("C.TXT").len(), 1);
        assert_eq!(tree.dir_count(), 2, "a and the implied a/b");
        assert_eq!(tree.single_root_dir(), None);
    }

    #[test]
    fn natural_sort() {
        let mut v = vec!["file10", "File2", "file1", "alpha"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["alpha", "file1", "File2", "file10"]);
    }
}
