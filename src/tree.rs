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

pub fn insert(root: &mut FolderNode, root_path: &Path, file: PathBuf, lang: Lang, lines: u64) {
    let _g = crate::perf::begin("tree.insert");
    let rel = file.strip_prefix(root_path).unwrap_or(&file).to_path_buf();
    let segments: Vec<String> = rel
        .iter()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .collect();
    if segments.is_empty() {
        return;
    }
    insert_rec(root, &segments, 0, file, lang, lines);
}

fn insert_rec(
    folder: &mut FolderNode,
    segs: &[String],
    i: usize,
    full_path: PathBuf,
    lang: Lang,
    lines: u64,
) {
    folder.total_lines += lines;
    folder.total_files += 1;
    *folder.lang_lines.entry(lang).or_insert(0) += lines;

    if i == segs.len() - 1 {
        folder.children.insert(
            segs[i].clone(),
            Node::File(FileNode {
                path: full_path,
                lang,
                lines,
            }),
        );
        return;
    }
    let entry = folder
        .children
        .entry(segs[i].clone())
        .or_insert_with(|| Node::Folder(FolderNode::default()));
    match entry {
        Node::Folder(child) => insert_rec(child, segs, i + 1, full_path, lang, lines),
        Node::File(_) => {
            // A file already exists at a path now claimed as a folder. The
            // walker shouldn't produce this — flag it loudly in debug builds.
            debug_assert!(
                false,
                "tree::insert: file/folder collision at {:?}",
                &segs[..=i]
            );
        }
    }
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

    fn fake(path: &str, lang: &'static str, lines: u64) -> (PathBuf, Lang, u64) {
        (PathBuf::from(path), Lang(lang), lines)
    }

    #[test]
    fn insert_builds_nested_folders() {
        let mut root = FolderNode::default();
        let base = Path::new("/r");
        let (p, l, n) = fake("/r/a/b/c.rs", "Rust", 10);
        insert(&mut root, base, p, l, n);
        let (p, l, n) = fake("/r/a/b/d.rs", "Rust", 20);
        insert(&mut root, base, p, l, n);
        let (p, l, n) = fake("/r/a/e.go", "Go", 5);
        insert(&mut root, base, p, l, n);

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
    fn resolve_walks_to_folder() {
        let mut root = FolderNode::default();
        let base = Path::new("/r");
        insert(
            &mut root,
            base,
            PathBuf::from("/r/x/y/f.rs"),
            Lang("Rust"),
            1,
        );

        let r = resolve(&root, &["x".into(), "y".into()]).unwrap();
        assert_eq!(r.total_files, 1);

        // resolve into a file path returns None.
        assert!(resolve(&root, &["x".into(), "y".into(), "f.rs".into()]).is_none());
    }

    #[test]
    fn collect_files_recurses() {
        let mut root = FolderNode::default();
        let base = Path::new("/r");
        insert(&mut root, base, PathBuf::from("/r/a.rs"), Lang("Rust"), 1);
        insert(
            &mut root,
            base,
            PathBuf::from("/r/sub/b.rs"),
            Lang("Rust"),
            2,
        );
        insert(
            &mut root,
            base,
            PathBuf::from("/r/empty.rs"),
            Lang("Rust"),
            0,
        );

        let mut out = Vec::new();
        collect_files(&root, &mut out);
        // Empty files (lines == 0) are skipped.
        assert_eq!(out.len(), 2);
    }
}
