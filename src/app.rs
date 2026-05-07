use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ratatui::layout::Rect;

use crate::lang::Lang;
use crate::tree::{self, FolderNode, Node};

#[derive(Debug, Default, Clone, Copy)]
pub struct LangStats {
    pub files: u64,
    pub lines: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Tree,
    Files,
    Nested,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TileTarget {
    Folder(Vec<String>),
    File(PathBuf),
}

#[derive(Debug, Clone, Copy)]
pub enum NavDir {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Default)]
pub struct Bench {
    pub enabled: bool,
    pub frame_durations: VecDeque<Duration>,
    pub last_treemap_layout: Duration,
    pub last_treemap_iters: u32,
    pub last_layout_excluded: u32,
    pub last_tiles_drawn: u32,
    pub last_halfblock: Duration,
    pub last_text_overlay: Duration,
    pub last_full_render: Duration,
    pub last_frame_alloc_bytes: u64,
    pub last_frame_allocs: u64,
    pub last_input_latency: Option<Duration>,
    pub total_count_nanos: u128,
    pub max_count_nanos: u64,
    pub min_count_nanos: u64,
    pub frames_rendered: u64,
}

impl Bench {
    pub fn record_frame(&mut self, d: Duration, alloc_bytes: u64, alloc_count: u64) {
        self.frames_rendered += 1;
        self.last_frame_alloc_bytes = alloc_bytes;
        self.last_frame_allocs = alloc_count;
        if self.frame_durations.len() >= 600 {
            self.frame_durations.pop_front();
        }
        self.frame_durations.push_back(d);
    }

    pub fn frame_stats(&self) -> Option<FrameStats> {
        if self.frame_durations.is_empty() {
            return None;
        }
        let mut sorted: Vec<u64> = self
            .frame_durations
            .iter()
            .map(|d| d.as_nanos() as u64)
            .collect();
        sorted.sort_unstable();
        let n = sorted.len();
        let avg = sorted.iter().sum::<u64>() / n as u64;
        let p50 = sorted[n / 2];
        let p95 = sorted[(n * 95) / 100];
        let p99 = sorted[(n * 99) / 100];
        let max = *sorted.last().unwrap();
        Some(FrameStats {
            avg: Duration::from_nanos(avg),
            p50: Duration::from_nanos(p50),
            p95: Duration::from_nanos(p95),
            p99: Duration::from_nanos(p99),
            max: Duration::from_nanos(max),
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct FrameStats {
    pub avg: Duration,
    pub p50: Duration,
    pub p95: Duration,
    pub p99: Duration,
    pub max: Duration,
}

pub struct App {
    pub root: PathBuf,
    pub started: Instant,
    pub stats: HashMap<Lang, LangStats>,
    pub tree: FolderNode,
    pub current_path: Vec<String>,
    pub view: View,
    pub last_tiles: Vec<(Rect, TileTarget)>,
    pub ranked_cache: Vec<(Lang, LangStats)>,
    pub stats_dirty: bool,
    pub items_dirty: bool,
    pub selected: Option<TileTarget>,
    pub legend_rect: Rect,
    pub legend_scroll: usize,
    pub legend_max_scroll: usize,
    pub bench: Bench,
    pub total_files: u64,
    pub total_lines: u64,
    pub total_bytes: u64,
    pub last_path: Option<PathBuf>,
    pub done: bool,
    pub finished_at: Option<Instant>,
}

impl App {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            started: Instant::now(),
            stats: HashMap::new(),
            tree: FolderNode::default(),
            current_path: Vec::new(),
            view: View::Tree,
            last_tiles: Vec::new(),
            ranked_cache: Vec::new(),
            stats_dirty: true,
            items_dirty: true,
            selected: None,
            legend_rect: Rect::default(),
            legend_scroll: 0,
            legend_max_scroll: 0,
            bench: Bench::default(),
            total_files: 0,
            total_lines: 0,
            total_bytes: 0,
            last_path: None,
            done: false,
            finished_at: None,
        }
    }

    pub fn record(&mut self, path: PathBuf, lang: Lang, lines: u64, bytes: u64, count_nanos: u64) {
        let _g = crate::perf::begin("app.record");
        if self.bench.enabled {
            self.bench.total_count_nanos += count_nanos as u128;
            if count_nanos > self.bench.max_count_nanos {
                self.bench.max_count_nanos = count_nanos;
            }
            if self.bench.min_count_nanos == 0 || count_nanos < self.bench.min_count_nanos {
                self.bench.min_count_nanos = count_nanos;
            }
        }
        self.record_inner(path, lang, lines, bytes);
    }

    fn record_inner(&mut self, path: PathBuf, lang: Lang, lines: u64, bytes: u64) {
        let s = self.stats.entry(lang).or_default();
        s.files += 1;
        s.lines += lines;
        s.bytes += bytes;
        self.total_files += 1;
        self.total_lines += lines;
        self.total_bytes += bytes;
        self.last_path = Some(path.clone());
        self.stats_dirty = true;
        self.items_dirty = true;
        if lines > 0 {
            tree::insert(&mut self.tree, &self.root, path, lang, lines);
        }
    }

    pub fn current_folder(&self) -> &FolderNode {
        tree::resolve(&self.tree, &self.current_path).unwrap_or(&self.tree)
    }

    pub fn up(&mut self) -> bool {
        if self.current_path.pop().is_some() {
            self.selected = None;
            self.items_dirty = true;
            true
        } else {
            false
        }
    }

    pub fn enter_selected(&mut self) {
        if let Some(TileTarget::Folder(path)) = self.selected.clone() {
            self.current_path = path;
            self.selected = self.first_visible_target();
            self.items_dirty = true;
        }
    }

    /// Pick the largest visible item in the current scope so callers can
    /// auto-select on a zoom or view change without waiting for a render
    /// to populate `last_tiles`. The choice mirrors what each view's
    /// builder would put first: largest direct child for tree/nested,
    /// largest file in scope for the files view.
    pub fn first_visible_target(&self) -> Option<TileTarget> {
        let folder = self.current_folder();
        match self.view {
            View::Tree | View::Nested => folder
                .children
                .iter()
                .filter_map(|(name, child)| match child {
                    Node::File(f) if f.lines > 0 => {
                        Some((f.lines, TileTarget::File(f.path.clone())))
                    }
                    Node::Folder(s) if s.total_lines > 0 => {
                        let mut p = self.current_path.clone();
                        p.push(name.clone());
                        Some((s.total_lines, TileTarget::Folder(p)))
                    }
                    _ => None,
                })
                .max_by_key(|(v, _)| *v)
                .map(|(_, t)| t),
            View::Files => {
                let mut files = Vec::new();
                tree::collect_files(folder, &mut files);
                files
                    .iter()
                    .max_by_key(|f| f.lines)
                    .map(|f| TileTarget::File(f.path.clone()))
            }
        }
    }

    pub fn toggle_view(&mut self) {
        self.view = match self.view {
            View::Tree => View::Files,
            View::Files => View::Nested,
            View::Nested => View::Tree,
        };
        self.selected = None;
        self.items_dirty = true;
    }

    pub fn navigate(&mut self, dir: NavDir) {
        if self.last_tiles.is_empty() {
            return;
        }
        let selected_target = match &self.selected {
            None => {
                self.selected = Some(self.last_tiles[0].1.clone());
                return;
            }
            Some(t) => t.clone(),
        };
        let current_rect = match self
            .last_tiles
            .iter()
            .find(|(_, target)| target == &selected_target)
        {
            Some((r, _)) => *r,
            None => {
                self.selected = Some(self.last_tiles[0].1.clone());
                return;
            }
        };

        // Edge-distance scoring. Containment (parent or child of the
        // current tile) yields a non-positive edge distance and is
        // filtered out — so navigation never jumps into a parent or
        // descends into the current tile's interior. When two candidates
        // share the same edge (e.g., a sibling folder and the first child
        // inside it both start where the current tile ends), the larger
        // tile wins via the area tiebreaker, so the user lands on the
        // outer container rather than its interior.
        let cur_x0 = current_rect.x as i32;
        let cur_y0 = current_rect.y as i32;
        let cur_x1 = cur_x0 + current_rect.width as i32;
        let cur_y1 = cur_y0 + current_rect.height as i32;

        type ScoreKey = (i32, i32, i32, i32);
        let mut best: Option<(ScoreKey, &TileTarget)> = None;
        for (rect, target) in &self.last_tiles {
            if *target == selected_target {
                continue;
            }
            let x0 = rect.x as i32;
            let y0 = rect.y as i32;
            let x1 = x0 + rect.width as i32;
            let y1 = y0 + rect.height as i32;

            let (edge, ortho_a0, ortho_a1, ortho_b0, ortho_b1) = match dir {
                NavDir::Right => (x0 - cur_x1, cur_y0, cur_y1, y0, y1),
                NavDir::Left => (cur_x0 - x1, cur_y0, cur_y1, y0, y1),
                NavDir::Down => (y0 - cur_y1, cur_x0, cur_x1, x0, x1),
                NavDir::Up => (cur_y0 - y1, cur_x0, cur_x1, x0, x1),
            };
            // edge < 0 means rect overlaps current along the primary axis
            // (or sits entirely behind it). A parent rect, a child rect,
            // or any rect behind the chosen direction all fall here.
            if edge < 0 {
                continue;
            }
            let overlap = ortho_a1.min(ortho_b1) - ortho_a0.max(ortho_b0);
            let ortho_bucket = if overlap > 0 { 0 } else { 1 };
            let ortho_gap = if overlap > 0 {
                0
            } else {
                (ortho_b0 - ortho_a1).max(ortho_a0 - ortho_b1)
            };
            // Bucket-first ordering: any tile with orthogonal overlap
            // beats every tile without; within a bucket, smaller edge
            // distance wins, then smaller orthogonal gap, then larger
            // area (so a sibling beats its first child when both share
            // the same edge).
            let area = -(rect.width as i32 * rect.height as i32);
            let key = (ortho_bucket, edge, ortho_gap, area);
            if best.map(|(b, _)| key < b).unwrap_or(true) {
                best = Some((key, target));
            }
        }
        if let Some((_, t)) = best {
            self.selected = Some(t.clone());
        }
    }

    /// Splits the breadcrumb into `(root, zoom)` so the header can render
    /// the scan root in a dim colour and the zoomed-in suffix bright. The
    /// root is tilde-shortened against `$HOME` when possible. `zoom`
    /// starts with `/` when non-empty.
    pub fn breadcrumb_parts(&self) -> (String, String) {
        let root = if let Ok(home) = std::env::var("HOME") {
            let home = std::path::PathBuf::from(home);
            if let Ok(rel) = self.root.strip_prefix(&home) {
                if rel.as_os_str().is_empty() {
                    "~".to_string()
                } else {
                    format!("~/{}", rel.display())
                }
            } else {
                self.root.display().to_string()
            }
        } else {
            self.root.display().to_string()
        };
        let mut zoom = String::new();
        for seg in &self.current_path {
            zoom.push('/');
            zoom.push_str(seg);
        }
        (root, zoom)
    }

    pub fn mark_done(&mut self, at: Instant) {
        if !self.done {
            self.done = true;
            self.finished_at = Some(at);
        }
    }

    pub fn elapsed_secs(&self) -> f64 {
        let end = self.finished_at.unwrap_or_else(Instant::now);
        end.duration_since(self.started).as_secs_f64()
    }

    pub fn ensure_ranked(&mut self) {
        if self.stats_dirty {
            self.ranked_cache.clear();
            self.ranked_cache
                .extend(self.stats.iter().map(|(l, s)| (*l, *s)));
            self.ranked_cache
                .sort_by_key(|(_, s)| std::cmp::Reverse(s.lines));
            self.stats_dirty = false;
        }
    }

    pub fn ranked(&self) -> &[(Lang, LangStats)] {
        &self.ranked_cache
    }

    pub fn hit(&self, x: u16, y: u16) -> Option<&TileTarget> {
        for (rect, target) in self.last_tiles.iter().rev() {
            if x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height {
                return Some(target);
            }
        }
        None
    }
}
