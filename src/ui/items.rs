use smelt_term::grid::Color;

use crate::app::{App, TileTarget, View};
use crate::format::fmt_int;
use crate::lang;
use crate::tree::{FolderNode, Node};

#[derive(Clone)]
pub(super) struct TileItem {
    pub(super) value: u64,
    pub(super) color: Color,
    pub(super) name: String,
    pub(super) subtitle1: String,
    pub(super) subtitle2: String,
    pub(super) target: TileTarget,
}

pub(super) fn build_items_into(app: &App, out: &mut Vec<TileItem>) {
    let _g = smelt_perf::perf::begin("ui.build_items");
    let folder = app.current_folder();
    let total = folder.total_lines.max(1);
    match app.view {
        View::Tree => build_tree_items(app, folder, total, out),
        View::Files => build_files_items(app, folder, total, out),
        View::Nested => {}
    }
}

pub(super) fn build_tree_items(
    app: &App,
    folder: &FolderNode,
    total: u64,
    out: &mut Vec<TileItem>,
) {
    out.reserve(folder.children.len());
    for (name, child) in &folder.children {
        match child {
            Node::File(file) => {
                if file.lines == 0 {
                    continue;
                }
                let pct = 100.0 * file.lines as f64 / total as f64;
                out.push(TileItem {
                    value: file.lines,
                    color: lang::color(file.lang),
                    name: name.clone(),
                    subtitle1: format!("{} ({:.1}%)", fmt_int(file.lines), pct),
                    subtitle2: String::new(),
                    target: TileTarget::File(file.path.clone()),
                });
            }
            Node::Folder(sub) => {
                if sub.total_lines == 0 {
                    continue;
                }
                let color = sub.dominant_lang().map(lang::color).unwrap_or(Color::Rgb {
                    r: 120,
                    g: 120,
                    b: 120,
                });
                let pct = 100.0 * sub.total_lines as f64 / total as f64;
                let mut path = app.current_path.clone();
                path.push(name.clone());
                out.push(TileItem {
                    value: sub.total_lines,
                    color,
                    name: format!("{name}/"),
                    subtitle1: format!("{} ({:.1}%)", fmt_int(sub.total_lines), pct),
                    subtitle2: format!("{} files", fmt_int(sub.total_files)),
                    target: TileTarget::Folder(path),
                });
            }
        }
    }
}

pub(super) const MAX_FILE_ITEMS: usize = 8192;

pub(super) fn build_files_items(
    app: &App,
    folder: &FolderNode,
    total: u64,
    out: &mut Vec<TileItem>,
) {
    let mut files = Vec::new();
    crate::tree::collect_files(folder, &mut files);
    files.sort_unstable_by_key(|f| std::cmp::Reverse(f.lines));
    files.truncate(MAX_FILE_ITEMS);

    let scope_path = app
        .current_path
        .iter()
        .fold(app.root.clone(), |p, s| p.join(s));

    out.reserve(files.len());
    for f in files {
        let pct = 100.0 * f.lines as f64 / total as f64;
        let rel = f.path.strip_prefix(&scope_path).unwrap_or(&f.path);
        let name = rel
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        let parent = rel
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        out.push(TileItem {
            value: f.lines,
            color: lang::color(f.lang),
            name,
            subtitle1: format!("{} ({:.1}%)", fmt_int(f.lines), pct),
            subtitle2: parent,
            target: TileTarget::File(f.path.clone()),
        });
    }
}
