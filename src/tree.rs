use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use crate::lang::Lang;

#[derive(Debug)]
pub enum Node {
    File(FileNode),
    Folder(FolderNode),
}

#[derive(Debug)]
pub struct FileNode {
    pub path: PathBuf,
    pub lang: Lang,
    pub lines: u64,
    pub bytes: u64,
}

#[derive(Debug, Default)]
pub struct FolderNode {
    pub children: BTreeMap<String, Node>,
    pub total_lines: u64,
    pub total_files: u64,
    pub lang_lines: HashMap<Lang, u64>,
}

impl FolderNode {
    pub fn dominant_lang(&self) -> Option<Lang> {
        self.lang_lines
            .iter()
            .max_by_key(|(_, n)| *n)
            .map(|(l, _)| *l)
    }
}

/// Result of a successful upsert: tells the caller whether this was the
/// file's first appearance (so App-level counters know to bump) and the
/// previous lines/lang/bytes if it already existed (so App-level stats
/// can subtract them before adding the new ones).
#[derive(Debug)]
pub enum UpsertOutcome {
    Inserted,
    Replaced {
        prev_lang: Lang,
        prev_lines: u64,
        prev_bytes: u64,
    },
}

/// Insert or replace a file at `file`'s path inside `root`. Folder
/// rollups (`total_lines`, `total_files`, `lang_lines`) are kept exact:
/// on replace, the previous file's contribution is subtracted before the
/// new one is added.
pub fn upsert(
    root: &mut FolderNode,
    root_path: &Path,
    file: PathBuf,
    lang: Lang,
    lines: u64,
    bytes: u64,
) -> Option<UpsertOutcome> {
    let _g = crate::perf::begin("tree.upsert");
    let segments = relative_segments(&file, root_path)?;
    Some(upsert_rec(root, &segments, 0, file, lang, lines, bytes))
}

fn upsert_rec(
    folder: &mut FolderNode,
    segs: &[String],
    i: usize,
    full_path: PathBuf,
    lang: Lang,
    lines: u64,
    bytes: u64,
) -> UpsertOutcome {
    if i == segs.len() - 1 {
        // Leaf: figure out the prev contribution (if any) so we can
        // produce an exact delta for this folder's rollups.
        let prev = match folder.children.get(&segs[i]) {
            Some(Node::File(f)) => Some((f.lang, f.lines, f.bytes)),
            _ => None,
        };
        match prev {
            Some((prev_lang, prev_lines, prev_bytes)) => {
                folder.total_lines = folder.total_lines + lines - prev_lines;
                *folder.lang_lines.entry(prev_lang).or_insert(0) =
                    folder.lang_lines[&prev_lang].saturating_sub(prev_lines);
                if folder.lang_lines[&prev_lang] == 0 {
                    folder.lang_lines.remove(&prev_lang);
                }
                *folder.lang_lines.entry(lang).or_insert(0) += lines;
                folder.children.insert(
                    segs[i].clone(),
                    Node::File(FileNode {
                        path: full_path,
                        lang,
                        lines,
                        bytes,
                    }),
                );
                return UpsertOutcome::Replaced {
                    prev_lang,
                    prev_lines,
                    prev_bytes,
                };
            }
            None => {
                folder.total_lines += lines;
                folder.total_files += 1;
                *folder.lang_lines.entry(lang).or_insert(0) += lines;
                folder.children.insert(
                    segs[i].clone(),
                    Node::File(FileNode {
                        path: full_path,
                        lang,
                        lines,
                        bytes,
                    }),
                );
                return UpsertOutcome::Inserted;
            }
        }
    }

    // Descend; we can't know the outcome until the leaf returns, so
    // apply rollups *after* the recursive call.
    let entry = folder
        .children
        .entry(segs[i].clone())
        .or_insert_with(|| Node::Folder(FolderNode::default()));
    let outcome = match entry {
        Node::Folder(child) => upsert_rec(child, segs, i + 1, full_path, lang, lines, bytes),
        Node::File(_) => {
            // A file already exists at a path now claimed as a folder.
            // The walker shouldn't produce this — flag in debug builds.
            debug_assert!(
                false,
                "tree::upsert: file/folder collision at {:?}",
                &segs[..=i]
            );
            return UpsertOutcome::Inserted;
        }
    };
    match &outcome {
        UpsertOutcome::Inserted => {
            folder.total_lines += lines;
            folder.total_files += 1;
            *folder.lang_lines.entry(lang).or_insert(0) += lines;
        }
        UpsertOutcome::Replaced {
            prev_lang,
            prev_lines,
            ..
        } => {
            folder.total_lines = folder.total_lines + lines - prev_lines;
            let entry = folder.lang_lines.entry(*prev_lang).or_insert(0);
            *entry = entry.saturating_sub(*prev_lines);
            if *entry == 0 {
                folder.lang_lines.remove(prev_lang);
            }
            *folder.lang_lines.entry(lang).or_insert(0) += lines;
        }
    }
    outcome
}

/// Result of a successful remove: the file's previous contribution, so
/// App-level stats can subtract it.
#[derive(Debug)]
pub struct Removed {
    pub lang: Lang,
    pub lines: u64,
    pub bytes: u64,
}

/// Remove the file at `file`'s path from `root`, rolling back its
/// contribution from every ancestor's totals. Empty folders along the
/// path are pruned. Returns `None` if no file existed there.
pub fn remove(root: &mut FolderNode, root_path: &Path, file: &Path) -> Option<Removed> {
    let _g = crate::perf::begin("tree.remove");
    let segments = relative_segments(file, root_path)?;
    remove_rec(root, &segments, 0)
}

fn remove_rec(folder: &mut FolderNode, segs: &[String], i: usize) -> Option<Removed> {
    if i == segs.len() - 1 {
        let removed = match folder.children.remove(&segs[i]) {
            Some(Node::File(f)) => Removed {
                lang: f.lang,
                lines: f.lines,
                bytes: f.bytes,
            },
            Some(other) => {
                // Put it back: caller asked to remove a path that's
                // actually a folder. No-op on stats.
                folder.children.insert(segs[i].clone(), other);
                return None;
            }
            None => return None,
        };
        folder.total_lines = folder.total_lines.saturating_sub(removed.lines);
        folder.total_files = folder.total_files.saturating_sub(1);
        if let Some(v) = folder.lang_lines.get_mut(&removed.lang) {
            *v = v.saturating_sub(removed.lines);
            if *v == 0 {
                folder.lang_lines.remove(&removed.lang);
            }
        }
        return Some(removed);
    }

    let removed = match folder.children.get_mut(&segs[i])? {
        Node::Folder(child) => remove_rec(child, segs, i + 1)?,
        Node::File(_) => return None,
    };
    folder.total_lines = folder.total_lines.saturating_sub(removed.lines);
    folder.total_files = folder.total_files.saturating_sub(1);
    if let Some(v) = folder.lang_lines.get_mut(&removed.lang) {
        *v = v.saturating_sub(removed.lines);
        if *v == 0 {
            folder.lang_lines.remove(&removed.lang);
        }
    }
    // Prune empty subfolders so the tree doesn't accumulate ghost dirs.
    if let Some(Node::Folder(child)) = folder.children.get(&segs[i])
        && child.children.is_empty()
    {
        folder.children.remove(&segs[i]);
    }
    Some(removed)
}

fn relative_segments(file: &Path, root_path: &Path) -> Option<Vec<String>> {
    let rel = file.strip_prefix(root_path).unwrap_or(file);
    let segs: Vec<String> = rel
        .iter()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .collect();
    if segs.is_empty() { None } else { Some(segs) }
}

pub fn resolve<'a>(root: &'a FolderNode, path: &[String]) -> Option<&'a FolderNode> {
    let mut cur = root;
    for seg in path {
        match cur.children.get(seg)? {
            Node::Folder(f) => cur = f,
            Node::File(_) => return None,
        }
    }
    Some(cur)
}

pub fn collect_files<'a>(folder: &'a FolderNode, out: &mut Vec<&'a FileNode>) {
    for child in folder.children.values() {
        match child {
            Node::File(f) => {
                if f.lines > 0 {
                    out.push(f);
                }
            }
            Node::Folder(sub) => collect_files(sub, out),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ins(
        root: &mut FolderNode,
        base: &Path,
        p: &str,
        lang: &'static str,
        lines: u64,
        bytes: u64,
    ) {
        upsert(root, base, PathBuf::from(p), Lang(lang), lines, bytes);
    }

    #[test]
    fn upsert_builds_nested_folders() {
        let mut root = FolderNode::default();
        let base = Path::new("/r");
        ins(&mut root, base, "/r/a/b/c.rs", "Rust", 10, 100);
        ins(&mut root, base, "/r/a/b/d.rs", "Rust", 20, 200);
        ins(&mut root, base, "/r/a/e.go", "Go", 5, 50);

        assert_eq!(root.total_files, 3);
        assert_eq!(root.total_lines, 35);

        let a = match root.children.get("a").unwrap() {
            Node::Folder(f) => f,
            _ => panic!(),
        };
        assert_eq!(a.total_files, 3);
        assert_eq!(a.total_lines, 35);
        assert_eq!(a.dominant_lang(), Some(Lang("Rust")));

        let b = match a.children.get("b").unwrap() {
            Node::Folder(f) => f,
            _ => panic!(),
        };
        assert_eq!(b.total_files, 2);
        assert_eq!(b.total_lines, 30);
    }

    #[test]
    fn upsert_replaces_with_correct_delta() {
        let mut root = FolderNode::default();
        let base = Path::new("/r");
        ins(&mut root, base, "/r/a/c.rs", "Rust", 10, 100);
        ins(&mut root, base, "/r/a/c.rs", "Rust", 25, 250);
        assert_eq!(root.total_files, 1);
        assert_eq!(root.total_lines, 25);
        let a = match root.children.get("a").unwrap() {
            Node::Folder(f) => f,
            _ => panic!(),
        };
        assert_eq!(a.lang_lines.get(&Lang("Rust")).copied(), Some(25));

        // Lang change subtracts from the old, adds to the new.
        ins(&mut root, base, "/r/a/c.rs", "Go", 7, 70);
        assert_eq!(root.total_lines, 7);
        let a = match root.children.get("a").unwrap() {
            Node::Folder(f) => f,
            _ => panic!(),
        };
        assert!(!a.lang_lines.contains_key(&Lang("Rust")));
        assert_eq!(a.lang_lines.get(&Lang("Go")).copied(), Some(7));
    }

    #[test]
    fn remove_subtracts_and_prunes_empty_folders() {
        let mut root = FolderNode::default();
        let base = Path::new("/r");
        ins(&mut root, base, "/r/a/b/c.rs", "Rust", 10, 100);
        ins(&mut root, base, "/r/a/b/d.rs", "Rust", 20, 200);

        let r = remove(&mut root, base, Path::new("/r/a/b/c.rs")).unwrap();
        assert_eq!(r.lines, 10);
        assert_eq!(root.total_files, 1);
        assert_eq!(root.total_lines, 20);

        // After removing the second file, the empty folders should be pruned.
        remove(&mut root, base, Path::new("/r/a/b/d.rs")).unwrap();
        assert!(root.children.is_empty());
    }

    #[test]
    fn resolve_walks_to_folder() {
        let mut root = FolderNode::default();
        let base = Path::new("/r");
        ins(&mut root, base, "/r/x/y/f.rs", "Rust", 1, 10);
        let r = resolve(&root, &["x".into(), "y".into()]).unwrap();
        assert_eq!(r.total_files, 1);
        assert!(resolve(&root, &["x".into(), "y".into(), "f.rs".into()]).is_none());
    }

    #[test]
    fn collect_files_recurses() {
        let mut root = FolderNode::default();
        let base = Path::new("/r");
        ins(&mut root, base, "/r/a.rs", "Rust", 1, 10);
        ins(&mut root, base, "/r/sub/b.rs", "Rust", 2, 20);
        ins(&mut root, base, "/r/empty.rs", "Rust", 0, 0);
        let mut out = Vec::new();
        collect_files(&root, &mut out);
        assert_eq!(out.len(), 2);
    }
}
