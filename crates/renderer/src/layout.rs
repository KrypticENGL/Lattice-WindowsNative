//! Force-directed layout, run on a worker thread.
//!
//! The simulation follows d3-force: velocity Verlet with an `alpha`
//! "temperature" that decays to zero, springs on edges, Barnes–Hut many-body
//! repulsion (θ = 0.9) and a weak pull toward the origin that keeps
//! disconnected components together. Each node is also pulled toward the
//! column of its rank along the edges' direction, so callers sit left of
//! the functions they call. It stops once alpha falls below
//! [`ALPHA_MIN`] (about 300 ticks from a cold start).
//!
//! The UI thread talks to it through [`LayoutWorker`]: commands go over a
//! channel; positions come back as [`Snapshot`]s in a shared slot, followed
//! by a call to the wake callback. At most ~60 snapshots per second are
//! published, and ticks are paced so a small graph still animates.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::geom::{Rect, Vec2};

pub const LINK_DISTANCE: f32 = 40.0;
const CHARGE: f32 = -60.0;
const THETA_SQ: f32 = 0.81;
const DISTANCE_MIN_SQ: f32 = 1.0;
const GRAVITY: f32 = 0.02;
const VELOCITY_KEEP: f32 = 0.6;
pub const ALPHA_MIN: f32 = 0.001;
/// Alpha reaches `ALPHA_MIN` from 1 in 300 ticks.
const ALPHA_DECAY: f32 = 0.0228;
/// Alpha while a node is being dragged.
const DRAG_ALPHA: f32 = 0.3;
/// Horizontal distance between rank columns.
const RANK_GAP: f32 = 90.0;
/// Pull toward the rank column (d3's forceX strength).
const RANK_STRENGTH: f32 = 0.15;
/// Spacing of the initial phyllotaxis spiral.
const INITIAL_RADIUS: f32 = 12.0;
const LEAF_CAPACITY: usize = 4;
const MAX_DEPTH: u32 = 24;
/// Graphs at least this large compute repulsion on several threads.
const PARALLEL_MIN_NODES: usize = 2000;

/// Plain force-directed simulation; [`LayoutWorker`] runs it off-thread.
pub struct Simulation {
    pos: Vec<Vec2>,
    vel: Vec<Vec2>,
    fixed: Vec<Option<Vec2>>,
    links: Vec<Link>,
    /// Target x of each node's rank column.
    column: Vec<f32>,
    alpha: f32,
    alpha_target: f32,
    tree: QuadTree,
    forces: Vec<Vec2>,
    threads: usize,
}

struct Link {
    a: u32,
    b: u32,
    strength: f32,
    /// Share of the correction applied to `b` (the lighter end moves more).
    bias: f32,
}

impl Simulation {
    /// `initial[i]` keeps a known position; `None` nodes are placed next to
    /// a placed neighbour, or on a spiral.
    pub fn new(initial: &[Option<Vec2>], edges: &[(u32, u32)], alpha: f32) -> Self {
        let n = initial.len();
        let mut pairs: Vec<(u32, u32)> = edges
            .iter()
            .filter(|(a, b)| a != b && (*a as usize) < n && (*b as usize) < n)
            .map(|&(a, b)| (a.min(b), a.max(b)))
            .collect();
        pairs.sort_unstable();
        pairs.dedup();
        let mut degree = vec![0u32; n];
        for &(a, b) in &pairs {
            degree[a as usize] += 1;
            degree[b as usize] += 1;
        }
        let links = pairs
            .iter()
            .map(|&(a, b)| {
                let (da, db) = (degree[a as usize] as f32, degree[b as usize] as f32);
                Link {
                    a,
                    b,
                    strength: 1.0 / da.min(db),
                    bias: da / (da + db),
                }
            })
            .collect();
        let threads = thread::available_parallelism()
            .map_or(1, |p| p.get())
            .clamp(1, 8);
        Self {
            pos: place(initial, &pairs),
            vel: vec![Vec2::ZERO; n],
            fixed: vec![None; n],
            links,
            column: columns(n, edges),
            alpha: alpha.clamp(0.0, 1.0),
            alpha_target: 0.0,
            tree: QuadTree::default(),
            forces: vec![Vec2::ZERO; n],
            threads,
        }
    }

    pub fn positions(&self) -> &[Vec2] {
        &self.pos
    }

    pub fn alpha(&self) -> f32 {
        self.alpha
    }

    pub fn is_settled(&self) -> bool {
        self.alpha < ALPHA_MIN && self.alpha_target < ALPHA_MIN
    }

    /// Restarts with at least `alpha` (re-run layout, reload).
    pub fn reheat(&mut self, alpha: f32) {
        self.alpha = self.alpha.max(alpha.clamp(0.0, 1.0));
    }

    /// Holds node `i` at `p` (dragging) and keeps the simulation warm.
    pub fn pin(&mut self, i: u32, p: Vec2) {
        if let Some(slot) = self.fixed.get_mut(i as usize) {
            if p.is_finite() {
                *slot = Some(p);
                self.alpha_target = DRAG_ALPHA;
            }
        }
    }

    /// Releases a pinned node; the layout cools down from here.
    pub fn release(&mut self, i: u32) {
        if let Some(slot) = self.fixed.get_mut(i as usize) {
            *slot = None;
        }
        if self.fixed.iter().all(Option::is_none) {
            self.alpha_target = 0.0;
        }
    }

    pub fn tick(&mut self) {
        self.alpha += (self.alpha_target - self.alpha) * ALPHA_DECAY;
        let alpha = self.alpha;
        self.apply_links(alpha);
        self.apply_charge(alpha);
        for i in 0..self.pos.len() {
            let v = &mut self.vel[i];
            *v = *v + self.forces[i] - self.pos[i] * (GRAVITY * alpha);
            v.x += (self.column[i] - self.pos[i].x) * (RANK_STRENGTH * alpha);
            if let Some(f) = self.fixed[i] {
                self.pos[i] = f;
                *v = Vec2::ZERO;
            } else {
                *v = *v * VELOCITY_KEEP;
                let next = self.pos[i] + *v;
                if next.is_finite() {
                    self.pos[i] = next;
                } else {
                    *v = Vec2::ZERO;
                }
            }
        }
    }

    fn apply_links(&mut self, alpha: f32) {
        for (k, l) in self.links.iter().enumerate() {
            let (a, b) = (l.a as usize, l.b as usize);
            let mut d = (self.pos[b] + self.vel[b]) - (self.pos[a] + self.vel[a]);
            if d.length_sq() == 0.0 {
                d = Vec2::new(jiggle(k as u64, 1), jiggle(k as u64, 2));
            }
            let len = d.length();
            let k = (len - LINK_DISTANCE) / len * alpha * l.strength;
            let d = d * k;
            self.vel[b] = self.vel[b] - d * l.bias;
            self.vel[a] = self.vel[a] + d * (1.0 - l.bias);
        }
    }

    fn apply_charge(&mut self, alpha: f32) {
        self.tree.build(&self.pos);
        let (tree, pos) = (&self.tree, &self.pos);
        let n = pos.len();
        if n >= PARALLEL_MIN_NODES && self.threads > 1 {
            let chunk = n.div_ceil(self.threads);
            thread::scope(|s| {
                for (c, out) in self.forces.chunks_mut(chunk).enumerate() {
                    s.spawn(move || {
                        for (k, f) in out.iter_mut().enumerate() {
                            *f = tree.force(pos, c * chunk + k, alpha);
                        }
                    });
                }
            });
        } else {
            for (i, f) in self.forces.iter_mut().enumerate() {
                *f = tree.force(pos, i, alpha);
            }
        }
    }
}

/// The x of each node's column: its rank is the longest path to it from a
/// node nothing points at, ignoring the edges that close a cycle. Columns
/// are centred on the origin.
fn columns(n: usize, edges: &[(u32, u32)]) -> Vec<f32> {
    let mut out: Vec<Vec<u32>> = vec![Vec::new(); n];
    for &(a, b) in edges {
        if a != b && (a as usize) < n && (b as usize) < n {
            out[a as usize].push(b);
        }
    }
    // Reverse DFS post-order is a topological order once back edges go.
    let mut seen = vec![false; n];
    let mut post = Vec::with_capacity(n);
    for root in 0..n {
        if seen[root] {
            continue;
        }
        seen[root] = true;
        let mut stack = vec![(root, 0usize)];
        while let Some((u, k)) = stack.last_mut() {
            if let Some(&v) = out[*u].get(*k) {
                *k += 1;
                if !seen[v as usize] {
                    seen[v as usize] = true;
                    stack.push((v as usize, 0));
                }
            } else {
                post.push(*u);
                stack.pop();
            }
        }
    }
    let mut order = vec![0usize; n];
    for (i, &u) in post.iter().rev().enumerate() {
        order[u] = i;
    }
    let mut rank = vec![0u32; n];
    for &u in post.iter().rev() {
        for &v in &out[u] {
            if order[v as usize] > order[u] {
                rank[v as usize] = rank[v as usize].max(rank[u] + 1);
            }
        }
    }
    let mid = rank.iter().max().map_or(0.0, |&m| m as f32 / 2.0);
    rank.iter().map(|&r| (r as f32 - mid) * RANK_GAP).collect()
}

/// Seeds positions: known ones are kept, a new node starts beside a placed
/// neighbour, the rest go on a phyllotaxis spiral around the origin.
pub(crate) fn place(initial: &[Option<Vec2>], pairs: &[(u32, u32)]) -> Vec<Vec2> {
    let mut pos: Vec<Option<Vec2>> = initial
        .iter()
        .map(|p| p.filter(|p| p.is_finite()))
        .collect();
    let any_known = pos.iter().any(Option::is_some);
    if any_known {
        // A couple of passes so chains of new nodes grow off placed ones.
        for pass in 0..3u64 {
            for (k, &(a, b)) in pairs.iter().enumerate() {
                let (a, b) = (a as usize, b as usize);
                // jiggle is ±1e-6, so this is up to ±10 world units.
                let off = Vec2::new(jiggle(k as u64, pass), jiggle(k as u64, pass + 7)) * 2e7;
                match (pos[a], pos[b]) {
                    (Some(p), None) => pos[b] = Some(p + off),
                    (None, Some(p)) => pos[a] = Some(p + off),
                    _ => {}
                }
            }
        }
    }
    let golden = std::f32::consts::PI * (3.0 - 5f32.sqrt());
    pos.iter()
        .enumerate()
        .map(|(i, p)| {
            p.unwrap_or_else(|| {
                let r = INITIAL_RADIUS * (0.5 + i as f32).sqrt();
                let a = i as f32 * golden;
                Vec2::new(r * a.cos(), r * a.sin())
            })
        })
        .collect()
}

/// A tiny deterministic offset (≈±1e-6) that separates coincident points.
fn jiggle(a: u64, b: u64) -> f32 {
    let mut h = a
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(b.wrapping_mul(0xC2B2_AE3D_27D4_EB4F));
    h ^= h >> 29;
    ((h & 0xFFFF) as f32 / 65535.0 - 0.5) * 1e-6
}

#[derive(Default)]
struct QuadTree {
    quads: Vec<Quad>,
    /// Point indices, grouped so each leaf owns a contiguous range.
    order: Vec<u32>,
}

#[derive(Clone, Copy)]
struct Quad {
    size: f32,
    center: Vec2,
    count: u32,
    /// Child quad indices; 0 means none (the root is never a child).
    children: [u32; 4],
    first: u32,
    len: u32,
}

impl QuadTree {
    fn build(&mut self, pos: &[Vec2]) {
        self.quads.clear();
        self.order.clear();
        self.order
            .extend((0..pos.len() as u32).filter(|&i| pos[i as usize].is_finite()));
        let Some(b) = Rect::bounding(pos) else {
            return;
        };
        let size = b.w.max(b.h).max(1.0);
        let len = self.order.len();
        self.subdivide(pos, 0, len, Vec2::new(b.x, b.y), size, 0);
    }

    /// Builds the quad for `order[lo..hi]` inside the square at `origin`
    /// and returns its index.
    fn subdivide(
        &mut self,
        pos: &[Vec2],
        lo: usize,
        hi: usize,
        origin: Vec2,
        size: f32,
        depth: u32,
    ) -> u32 {
        let mut sum = Vec2::ZERO;
        for &i in &self.order[lo..hi] {
            sum = sum + pos[i as usize];
        }
        let index = self.quads.len() as u32;
        self.quads.push(Quad {
            size,
            center: sum * (1.0 / (hi - lo).max(1) as f32),
            count: (hi - lo) as u32,
            children: [0; 4],
            first: lo as u32,
            len: (hi - lo) as u32,
        });
        if hi - lo <= LEAF_CAPACITY || depth >= MAX_DEPTH || size < 1e-3 {
            return index;
        }
        let half = size / 2.0;
        let mid = origin + Vec2::new(half, half);
        // Split by y, then each half by x: quadrants TL, TR, BL, BR.
        let ys = lo + partition(&mut self.order[lo..hi], |i| pos[i as usize].y < mid.y);
        let xs_top = lo + partition(&mut self.order[lo..ys], |i| pos[i as usize].x < mid.x);
        let xs_bot = ys + partition(&mut self.order[ys..hi], |i| pos[i as usize].x < mid.x);
        let ranges = [
            (lo, xs_top, origin),
            (xs_top, ys, Vec2::new(mid.x, origin.y)),
            (ys, xs_bot, Vec2::new(origin.x, mid.y)),
            (xs_bot, hi, mid),
        ];
        for (q, (a, b, o)) in ranges.into_iter().enumerate() {
            if b > a {
                let child = self.subdivide(pos, a, b, o, half, depth + 1);
                self.quads[index as usize].children[q] = child;
            }
        }
        index
    }

    /// Repulsion on point `i` (a velocity change).
    fn force(&self, pos: &[Vec2], i: usize, alpha: f32) -> Vec2 {
        let mut f = Vec2::ZERO;
        let p = pos[i];
        if self.quads.is_empty() || !p.is_finite() {
            return f;
        }
        let mut stack = [0u32; 4 * MAX_DEPTH as usize + 4];
        let mut top = 1;
        while top > 0 {
            top -= 1;
            let q = &self.quads[stack[top] as usize];
            let d = q.center - p;
            let l = d.length_sq();
            if q.size * q.size < THETA_SQ * l {
                // Far enough: treat the whole quad as one body.
                f = f + d * (CHARGE * q.count as f32 * alpha / soften(l));
                continue;
            }
            if q.children.iter().any(|&c| c != 0) {
                for &c in &q.children {
                    if c != 0 {
                        stack[top] = c;
                        top += 1;
                    }
                }
                continue;
            }
            for &j in &self.order[q.first as usize..(q.first + q.len) as usize] {
                if j as usize == i {
                    continue;
                }
                let mut d = pos[j as usize] - p;
                if d.length_sq() == 0.0 {
                    d = Vec2::new(jiggle(i as u64, j.into()), jiggle(j.into(), i as u64));
                }
                f = f + d * (CHARGE * alpha / soften(d.length_sq()));
            }
        }
        f
    }
}

/// d3's minimum-distance softening: below `DISTANCE_MIN`, `l` becomes
/// `sqrt(DISTANCE_MIN² · l)` so nearby points do not explode apart.
fn soften(l: f32) -> f32 {
    if l < DISTANCE_MIN_SQ {
        (DISTANCE_MIN_SQ * l).sqrt().max(f32::MIN_POSITIVE)
    } else {
        l
    }
}

/// Moves elements matching `pred` to the front; returns how many matched.
fn partition(items: &mut [u32], pred: impl Fn(u32) -> bool) -> usize {
    let mut k = 0;
    for j in 0..items.len() {
        if pred(items[j]) {
            items.swap(k, j);
            k += 1;
        }
    }
    k
}

/// Positions published by the worker.
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// The `generation` passed to [`LayoutWorker::load`].
    pub generation: u64,
    pub positions: Arc<[Vec2]>,
    /// `false` once the layout has settled; the UI can stop repainting.
    pub running: bool,
}

enum Command {
    Load {
        generation: u64,
        initial: Vec<Option<Vec2>>,
        edges: Vec<(u32, u32)>,
        alpha: f32,
    },
    Pin(u32, Vec2),
    Release(u32),
    Reheat(f32),
    Shutdown,
}

/// Publish at most this often.
const PUBLISH_INTERVAL: Duration = Duration::from_millis(16);
/// Ticks are spaced at least this far apart, so small graphs animate.
const MIN_TICK: Duration = Duration::from_millis(4);

type Slot = Arc<Mutex<Option<Snapshot>>>;

/// Owns the `graph-layout` thread. Dropping it stops the thread.
pub struct LayoutWorker {
    commands: Sender<Command>,
    latest: Slot,
    thread: Option<JoinHandle<()>>,
}

impl LayoutWorker {
    /// Starts the thread. `wake` is called (from the worker) after each
    /// published snapshot.
    pub fn spawn(wake: Box<dyn Fn() + Send>) -> std::io::Result<Self> {
        let (commands, rx) = mpsc::channel();
        let latest: Slot = Arc::default();
        let slot = latest.clone();
        let thread = thread::Builder::new()
            .name("graph-layout".into())
            .spawn(move || run(&rx, &slot, &*wake))?;
        Ok(Self {
            commands,
            latest,
            thread: Some(thread),
        })
    }

    /// Replaces the simulated graph. `initial[i]` keeps a node's position.
    pub fn load(
        &self,
        generation: u64,
        initial: Vec<Option<Vec2>>,
        edges: Vec<(u32, u32)>,
        alpha: f32,
    ) {
        let _ = self.commands.send(Command::Load {
            generation,
            initial,
            edges,
            alpha,
        });
    }

    pub fn pin(&self, node: u32, pos: Vec2) {
        let _ = self.commands.send(Command::Pin(node, pos));
    }

    pub fn release(&self, node: u32) {
        let _ = self.commands.send(Command::Release(node));
    }

    pub fn reheat(&self, alpha: f32) {
        let _ = self.commands.send(Command::Reheat(alpha));
    }

    /// The newest snapshot since the last call, if any.
    pub fn take(&self) -> Option<Snapshot> {
        self.latest
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }
}

impl Drop for LayoutWorker {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(rx: &Receiver<Command>, slot: &Slot, wake: &dyn Fn()) {
    let mut sim: Option<(u64, Simulation)> = None;
    let mut last_publish = Instant::now();
    let mut next_tick = Instant::now();
    loop {
        let active = sim.as_ref().is_some_and(|(_, s)| !s.is_settled());
        // Wait for a command: forever when idle, until the next tick when
        // running.
        let first = if active {
            match rx.recv_timeout(next_tick.saturating_duration_since(Instant::now())) {
                Ok(c) => Some(c),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        } else {
            match rx.recv() {
                Ok(c) => Some(c),
                Err(_) => return,
            }
        };
        let mut pending = first.into_iter().collect::<Vec<_>>();
        loop {
            match rx.try_recv() {
                Ok(c) => pending.push(c),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        let mut changed = false;
        for cmd in pending {
            match cmd {
                Command::Shutdown => return,
                Command::Load {
                    generation,
                    initial,
                    edges,
                    alpha,
                } => {
                    sim = Some((generation, Simulation::new(&initial, &edges, alpha)));
                    changed = true;
                }
                Command::Pin(i, p) => {
                    if let Some((_, s)) = sim.as_mut() {
                        s.pin(i, p);
                    }
                }
                Command::Release(i) => {
                    if let Some((_, s)) = sim.as_mut() {
                        s.release(i);
                    }
                }
                Command::Reheat(a) => {
                    if let Some((_, s)) = sim.as_mut() {
                        s.reheat(a);
                    }
                }
            }
        }
        let Some((generation, s)) = sim.as_mut() else {
            continue;
        };
        if Instant::now() < next_tick && !changed {
            continue;
        }
        if !s.is_settled() {
            s.tick();
            next_tick = Instant::now() + MIN_TICK;
        }
        let settled = s.is_settled();
        if changed || settled || last_publish.elapsed() >= PUBLISH_INTERVAL {
            last_publish = Instant::now();
            let snapshot = Snapshot {
                generation: *generation,
                positions: Arc::from(s.positions()),
                running: !settled,
            };
            *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(snapshot);
            wake();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::GraphModel;

    fn edges_of(m: &GraphModel) -> Vec<(u32, u32)> {
        m.edges().iter().map(|e| (e.from, e.to)).collect()
    }

    fn settle(sim: &mut Simulation) -> usize {
        let mut ticks = 0;
        while !sim.is_settled() && ticks < 2000 {
            sim.tick();
            ticks += 1;
        }
        ticks
    }

    #[test]
    fn converges_deterministically_without_nan() {
        let m = GraphModel::synthetic(300, 2.0);
        let run = || {
            let mut s = Simulation::new(&vec![None; 300], &edges_of(&m), 1.0);
            let ticks = settle(&mut s);
            (ticks, s.positions().to_vec())
        };
        let (ticks, a) = run();
        let (_, b) = run();
        assert!((250..=400).contains(&ticks), "{ticks} ticks");
        assert_eq!(a, b);
        assert!(a.iter().all(|p| p.is_finite()));
    }

    #[test]
    fn linked_nodes_end_closer_than_unlinked() {
        // Two triangles, no edges between them.
        let edges = [(0, 1), (1, 2), (2, 0), (3, 4), (4, 5), (5, 3)];
        let mut s = Simulation::new(&[None; 6], &edges, 1.0);
        settle(&mut s);
        let p = s.positions();
        let d = |a: usize, b: usize| (p[a] - p[b]).length();
        assert!(d(0, 1) < d(0, 4));
        assert!((d(0, 1) - LINK_DISTANCE).abs() < LINK_DISTANCE);
    }

    #[test]
    fn callers_end_left_of_callees_even_with_a_cycle() {
        // 0 calls 1 and 2, 1 calls 2, 2 calls back into 1.
        let edges = [(0, 1), (0, 2), (1, 2), (2, 1)];
        assert_eq!(columns(3, &edges), vec![-RANK_GAP, 0.0, RANK_GAP]);
        let mut s = Simulation::new(&[None; 3], &edges, 1.0);
        settle(&mut s);
        let p = s.positions();
        assert!(p[0].x < p[1].x && p[1].x < p[2].x, "{p:?}");
    }

    #[test]
    fn degenerate_inputs() {
        for n in [0usize, 1, 2] {
            let mut s = Simulation::new(&vec![None; n], &[(0, 0), (0, 9)], 1.0);
            settle(&mut s);
            assert!(s.positions().iter().all(|p| p.is_finite()));
        }
        // Everything starts at one point.
        let mut s = Simulation::new(&[Some(Vec2::new(5.0, 5.0)); 50], &[(0, 1)], 1.0);
        settle(&mut s);
        let p = s.positions();
        assert!(p.iter().all(|p| p.is_finite()));
        assert!((p[0] - p[1]).length() > 1.0);
    }

    #[test]
    fn pinned_node_stays_and_known_positions_are_kept() {
        let initial = [Some(Vec2::new(100.0, 0.0)), None, None];
        let mut s = Simulation::new(&initial, &[(0, 1), (1, 2)], 0.0);
        // Not started: positions are the seeds, new nodes near neighbours.
        assert_eq!(s.positions()[0], Vec2::new(100.0, 0.0));
        assert!((s.positions()[1] - s.positions()[0]).length() < LINK_DISTANCE);
        s.pin(2, Vec2::new(-50.0, 20.0));
        for _ in 0..50 {
            s.tick();
        }
        assert_eq!(s.positions()[2], Vec2::new(-50.0, 20.0));
        assert!(!s.is_settled());
        s.release(2);
        settle(&mut s);
        assert!(s.is_settled());
    }

    #[test]
    fn worker_publishes_until_settled() {
        let (tx, rx) = mpsc::channel();
        let w = LayoutWorker::spawn(Box::new(move || {
            let _ = tx.send(());
        }))
        .expect("spawn");
        let m = GraphModel::synthetic(50, 1.5);
        w.load(7, vec![None; 50], edges_of(&m), 1.0);
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut last = None;
        while Instant::now() < deadline {
            if rx.recv_timeout(Duration::from_secs(5)).is_err() {
                break;
            }
            if let Some(s) = w.take() {
                assert_eq!(s.generation, 7);
                assert_eq!(s.positions.len(), 50);
                let done = !s.running;
                last = Some(s);
                if done {
                    break;
                }
            }
        }
        assert!(last.is_some_and(|s| !s.running), "layout never settled");
        drop(w);
    }

    #[test]
    fn large_graph_ticks_in_parallel() {
        let m = GraphModel::synthetic(PARALLEL_MIN_NODES + 500, 2.5);
        let n = m.nodes().len();
        let mut s = Simulation::new(&vec![None; n], &edges_of(&m), 1.0);
        for _ in 0..5 {
            s.tick();
        }
        assert!(s.positions().iter().all(|p| p.is_finite()));
    }
}
