//! The Feather PDF logo: a white feather on a rounded teal-to-blue tile, drawn from
//! vector shapes with an anti-aliasing scanline rasterizer.

/// Sizes embedded in the icon. Windows picks the closest one for each place it shows it.
pub const ICON_SIZES: [u32; 10] = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256];

/// Brand gradient, top-left to bottom-right.
const TILE_FROM: [f64; 3] = [0x2D as f64 / 255.0, 0xD4 as f64 / 255.0, 0xB4 as f64 / 255.0];
const TILE_TO: [f64; 3] = [0x0B as f64 / 255.0, 0x5E as f64 / 255.0, 0x8C as f64 / 255.0];
const WHITE: [f64; 3] = [1.0, 1.0, 1.0];

type Pt = (f64, f64);

/// Renders the logo at `size` x `size`. Returns straight-alpha RGBA, top row first.
pub fn render(size: u32) -> Vec<u8> {
    let n = size as f64;
    let mut canvas = Canvas::new(size as usize);
    let tile_color = |x: f64, y: f64| {
        let t = ((x + y) / (2.0 * n)).clamp(0.0, 1.0);
        [0, 1, 2].map(|i| TILE_FROM[i] + (TILE_TO[i] - TILE_FROM[i]) * t)
    };

    let m = n * 0.06;
    canvas.fill(&[rounded_rect(m, m, n - 2.0 * m, n - 2.0 * m, n * 0.22)], &tile_color);

    let feather = Feather::new(n);
    canvas.fill(&[feather.vane()], &|_, _| WHITE);
    // The shaft and the notches are cut out of the vane in the tile's color.
    let mut cuts = vec![feather.shaft()];
    cuts.extend(feather.notches());
    canvas.fill(&cuts, &tile_color);
    canvas.fill(&[feather.quill()], &|_, _| WHITE);

    canvas.into_rgba()
}

/// The feather is laid out along a curved spine (a quadratic Bezier) from the quill at the
/// bottom-left to the tip at the top-right. Points are given as (position along the spine,
/// signed distance from it), in pixels.
struct Feather {
    size: f64,
    p0: Pt,
    p1: Pt,
    p2: Pt,
    /// Where the vane starts along the spine.
    vane_start: f64,
    /// Width of the shaft line; at least a pixel and a half so it survives at 16 px.
    shaft_w: f64,
}

impl Feather {
    fn new(n: f64) -> Feather {
        Feather {
            size: n,
            p0: (0.27 * n, 0.80 * n),
            p1: (0.36 * n, 0.36 * n),
            p2: (0.79 * n, 0.19 * n),
            vane_start: 0.10,
            shaft_w: (0.028 * n).max(1.5),
        }
    }

    fn spine(&self, t: f64) -> Pt {
        let (a, b, c) = ((1.0 - t) * (1.0 - t), 2.0 * (1.0 - t) * t, t * t);
        (a * self.p0.0 + b * self.p1.0 + c * self.p2.0, a * self.p0.1 + b * self.p1.1 + c * self.p2.1)
    }

    fn normal(&self, t: f64) -> Pt {
        let dx = 2.0 * (1.0 - t) * (self.p1.0 - self.p0.0) + 2.0 * t * (self.p2.0 - self.p1.0);
        let dy = 2.0 * (1.0 - t) * (self.p1.1 - self.p0.1) + 2.0 * t * (self.p2.1 - self.p1.1);
        let len = (dx * dx + dy * dy).sqrt();
        (-dy / len, dx / len)
    }

    fn at(&self, t: f64, offset: f64) -> Pt {
        let (p, nrm) = (self.spine(t), self.normal(t));
        (p.0 + nrm.0 * offset, p.1 + nrm.1 * offset)
    }

    /// Vane half-widths at spine position t: (upper side, lower side).
    fn widths(&self, t: f64) -> (f64, f64) {
        let u = ((t - self.vane_start) / (1.0 - self.vane_start)).clamp(0.0, 1.0);
        let f = (std::f64::consts::PI * u.powf(0.62)).sin().max(0.0).powf(0.85);
        (0.20 * self.size * f, 0.13 * self.size * f)
    }

    fn vane(&self) -> Vec<Pt> {
        const STEPS: usize = 48;
        let ts: Vec<f64> = (0..=STEPS).map(|i| self.vane_start + (1.0 - self.vane_start) * i as f64 / STEPS as f64).collect();
        let mut pts: Vec<Pt> = ts.iter().map(|&t| self.at(t, -self.widths(t).0)).collect();
        pts.extend(ts.iter().rev().map(|&t| self.at(t, self.widths(t).1)));
        pts
    }

    /// The central line through the vane, tapering toward the tip.
    fn shaft(&self) -> Vec<Pt> {
        self.strip(self.vane_start + 0.02, 0.9, |k| self.shaft_w * (1.0 - 0.6 * k))
    }

    /// The quill below the vane, slightly wider and with a tapered end.
    fn quill(&self) -> Vec<Pt> {
        self.strip(-0.07, self.vane_start + 0.04, |k| self.shaft_w * 1.25 * (0.55 + 0.45 * k.min(0.4) / 0.4))
    }

    /// A band of width `w(k)` along the spine from t0 to t1, where k runs 0..1.
    fn strip(&self, t0: f64, t1: f64, w: impl Fn(f64) -> f64) -> Vec<Pt> {
        const STEPS: usize = 24;
        let ks: Vec<f64> = (0..=STEPS).map(|i| i as f64 / STEPS as f64).collect();
        let t = |k: f64| t0 + (t1 - t0) * k;
        let mut pts: Vec<Pt> = ks.iter().map(|&k| self.at(t(k), w(k) / 2.0)).collect();
        pts.extend(ks.iter().rev().map(|&k| self.at(t(k), -w(k) / 2.0)));
        pts
    }

    /// V-shaped cuts into the edges, slanting toward the quill like separated barbs.
    fn notches(&self) -> Vec<Vec<Pt>> {
        let cut = |t_out: f64, t_in: f64, upper: bool, depth: f64| {
            let side = |t: f64| if upper { -self.widths(t).0 } else { self.widths(t).1 };
            let out = |t: f64| side(t) * 1.4; // past the edge so the cut fully crosses it
            let gap = 0.05;
            vec![self.at(t_out - gap, out(t_out - gap)), self.at(t_out + gap, out(t_out + gap)), self.at(t_in, side(t_in) * depth)]
        };
        vec![cut(0.62, 0.52, true, 0.35), cut(0.40, 0.31, false, 0.30)]
    }
}

fn rounded_rect(x: f64, y: f64, w: f64, h: f64, r: f64) -> Vec<Pt> {
    const STEPS: usize = 12;
    let corners = [(x + w - r, y + r, -90.0), (x + w - r, y + h - r, 0.0), (x + r, y + h - r, 90.0), (x + r, y + r, 180.0)];
    let mut pts = Vec::new();
    for (cx, cy, start) in corners {
        for i in 0..=STEPS {
            let a = (start + 90.0 * i as f64 / STEPS as f64).to_radians();
            pts.push((cx + r * a.cos(), cy + r * a.sin()));
        }
    }
    pts
}

/// Straight-alpha RGBA canvas with "source over" compositing.
struct Canvas {
    size: usize,
    /// Premultiplied RGBA.
    px: Vec<[f64; 4]>,
}

impl Canvas {
    fn new(size: usize) -> Canvas {
        Canvas { size, px: vec![[0.0; 4]; size * size] }
    }

    /// Fills the union of `polys` (non-zero winding) with `color(x, y)`.
    fn fill(&mut self, polys: &[Vec<Pt>], color: &dyn Fn(f64, f64) -> [f64; 3]) {
        let cov = coverage(self.size, polys);
        for (i, &a) in cov.iter().enumerate() {
            if a <= 0.0 {
                continue;
            }
            let a = a.min(1.0);
            let (x, y) = ((i % self.size) as f64 + 0.5, (i / self.size) as f64 + 0.5);
            let c = color(x, y);
            let d = &mut self.px[i];
            for k in 0..3 {
                d[k] = c[k] * a + d[k] * (1.0 - a);
            }
            d[3] = a + d[3] * (1.0 - a);
        }
    }

    fn into_rgba(self) -> Vec<u8> {
        self.px
            .iter()
            .flat_map(|p| {
                let a = p[3];
                let un = |v: f64| if a > 0.0 { (v / a * 255.0).round().clamp(0.0, 255.0) as u8 } else { 0 };
                [un(p[0]), un(p[1]), un(p[2]), (a * 255.0).round() as u8]
            })
            .collect()
    }
}

/// Fraction of each pixel covered by the polygons: 16 sample rows per pixel, exact
/// horizontal coverage within each row.
fn coverage(size: usize, polys: &[Vec<Pt>]) -> Vec<f64> {
    const ROWS: usize = 16;
    let mut cov = vec![0.0; size * size];
    let edges: Vec<(Pt, Pt)> = polys.iter().flat_map(|p| (0..p.len()).map(move |i| (p[i], p[(i + 1) % p.len()]))).collect();
    let mut crossings: Vec<(f64, i32)> = Vec::new();
    for row in 0..size * ROWS {
        let y = (row as f64 + 0.5) / ROWS as f64;
        crossings.clear();
        for &((x0, y0), (x1, y1)) in &edges {
            if (y0 <= y) != (y1 <= y) {
                let x = x0 + (y - y0) / (y1 - y0) * (x1 - x0);
                crossings.push((x, if y1 > y0 { 1 } else { -1 }));
            }
        }
        crossings.sort_by(|a, b| a.0.total_cmp(&b.0));
        let line = &mut cov[(row / ROWS) * size..(row / ROWS + 1) * size];
        let (mut winding, mut start) = (0, 0.0);
        for &(x, dir) in &crossings {
            let before = winding;
            winding += dir;
            if before == 0 && winding != 0 {
                start = x;
            } else if before != 0 && winding == 0 {
                add_span(line, start, x, 1.0 / ROWS as f64);
            }
        }
    }
    cov
}

fn add_span(line: &mut [f64], x0: f64, x1: f64, weight: f64) {
    let (x0, x1) = (x0.max(0.0), x1.min(line.len() as f64));
    if x1 <= x0 {
        return;
    }
    for px in x0.floor() as usize..(x1.ceil() as usize).min(line.len()) {
        let overlap = x1.min(px as f64 + 1.0) - x0.max(px as f64);
        if overlap > 0.0 {
            line[px] += overlap * weight;
        }
    }
}
