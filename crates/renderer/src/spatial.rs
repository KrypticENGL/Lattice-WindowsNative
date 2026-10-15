//! Uniform grid over node positions for hit testing and culling. Rebuilt
//! whenever a new layout snapshot arrives (O(n)).

use crate::geom::{Rect, Vec2};

/// At most this many cells per axis, so sparse outliers cannot blow up
/// memory.
const MAX_CELLS_PER_AXIS: usize = 512;

#[derive(Debug, Default)]
pub struct Grid {
    origin: Vec2,
    /// Bounding box of the indexed points.
    bounds: Rect,
    cell: f32,
    cols: usize,
    rows: usize,
    /// CSR: items of cell `c` are `items[start[c]..start[c + 1]]`.
    start: Vec<u32>,
    items: Vec<u32>,
}

impl Grid {
    /// Indexes `points` in cells of roughly `cell` world units.
    pub fn build(points: &[Vec2], cell: f32) -> Self {
        let Some(bounds) = Rect::bounding(points) else {
            return Self::default();
        };
        let span = bounds.w.max(bounds.h);
        let cell = cell
            .max(span / MAX_CELLS_PER_AXIS as f32)
            .max(f32::MIN_POSITIVE);
        let cols = ((bounds.w / cell) as usize + 1).min(MAX_CELLS_PER_AXIS);
        let rows = ((bounds.h / cell) as usize + 1).min(MAX_CELLS_PER_AXIS);
        let mut grid = Self {
            origin: Vec2::new(bounds.x, bounds.y),
            bounds,
            cell,
            cols,
            rows,
            start: vec![0; cols * rows + 1],
            items: Vec::new(),
        };
        let cells: Vec<Option<usize>> = points.iter().map(|&p| grid.cell_of(p)).collect();
        for c in cells.iter().flatten() {
            grid.start[c + 1] += 1;
        }
        for i in 0..cols * rows {
            grid.start[i + 1] += grid.start[i];
        }
        let mut fill = grid.start.clone();
        grid.items = vec![0; grid.start[cols * rows] as usize];
        for (i, c) in cells.iter().enumerate() {
            if let Some(c) = *c {
                grid.items[fill[c] as usize] = i as u32;
                fill[c] += 1;
            }
        }
        grid
    }

    fn cell_of(&self, p: Vec2) -> Option<usize> {
        if !p.is_finite() {
            return None;
        }
        let cx = ((p.x - self.origin.x) / self.cell).floor();
        let cy = ((p.y - self.origin.y) / self.cell).floor();
        let cx = (cx.max(0.0) as usize).min(self.cols - 1);
        let cy = (cy.max(0.0) as usize).min(self.rows - 1);
        Some(cy * self.cols + cx)
    }

    /// Cell index range covering `r`, clamped to the grid.
    fn cell_range(&self, r: Rect) -> Option<(usize, usize, usize, usize)> {
        if self.cols == 0 {
            return None;
        }
        let b = self.bounds;
        if r.right() < b.x || r.bottom() < b.y || r.x > b.right() || r.y > b.bottom() {
            return None;
        }
        let c = |v: f32, o: f32, n: usize| (((v - o) / self.cell).max(0.0) as usize).min(n - 1);
        Some((
            c(r.x, self.origin.x, self.cols),
            c(r.right(), self.origin.x, self.cols),
            c(r.y, self.origin.y, self.rows),
            c(r.bottom(), self.origin.y, self.rows),
        ))
    }

    /// Calls `f` with every indexed point whose cell overlaps `r` (a
    /// superset of the points inside `r`).
    pub fn query(&self, r: Rect, mut f: impl FnMut(u32)) {
        let Some((x0, x1, y0, y1)) = self.cell_range(r) else {
            return;
        };
        for cy in y0..=y1 {
            let row = cy * self.cols;
            let a = self.start[row + x0] as usize;
            let b = self.start[row + x1 + 1] as usize;
            for &i in &self.items[a..b] {
                f(i);
            }
        }
    }

    /// The point nearest to `p` within `radius`, if any.
    pub fn nearest(&self, points: &[Vec2], p: Vec2, radius: f32) -> Option<u32> {
        let mut best: Option<(f32, u32)> = None;
        self.query(Rect::new(p.x, p.y, 0.0, 0.0).inflate(radius), |i| {
            if let Some(&q) = points.get(i as usize) {
                let d = (q - p).length_sq();
                if d <= radius * radius && best.is_none_or(|(bd, _)| d < bd) {
                    best = Some((d, i));
                }
            }
        });
        best.map(|(_, i)| i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_and_query() {
        let pts: Vec<Vec2> = (0..100)
            .map(|i| Vec2::new((i % 10) as f32 * 50.0, (i / 10) as f32 * 50.0))
            .collect();
        let g = Grid::build(&pts, 40.0);
        assert_eq!(g.nearest(&pts, Vec2::new(103.0, 52.0), 10.0), Some(12));
        assert_eq!(g.nearest(&pts, Vec2::new(125.0, 25.0), 10.0), None);
        let mut hits = Vec::new();
        g.query(Rect::new(90.0, 90.0, 70.0, 70.0), |i| hits.push(i));
        for want in [22, 23, 32, 33] {
            assert!(hits.contains(&want), "{want}");
        }
        let mut none = Vec::new();
        g.query(Rect::new(-500.0, -500.0, 10.0, 10.0), |i| none.push(i));
        assert!(none.is_empty());
    }

    #[test]
    fn degenerate_inputs() {
        let g = Grid::build(&[], 10.0);
        assert_eq!(g.nearest(&[], Vec2::ZERO, 5.0), None);
        let same = vec![Vec2::new(3.0, 3.0); 50];
        let g = Grid::build(&same, 10.0);
        assert!(g.nearest(&same, Vec2::new(3.0, 3.5), 1.0).is_some());
        let wild = [
            Vec2::new(0.0, 0.0),
            Vec2::new(1e9, -1e9),
            Vec2::new(f32::NAN, 1.0),
        ];
        let g = Grid::build(&wild, 10.0);
        assert_eq!(g.nearest(&wild, Vec2::new(1e9, -1e9), 1e7), Some(1));
    }
}
