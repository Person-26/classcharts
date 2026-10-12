//! Interactive prerequisite flowchart viewer.
//!
//! Reads the graph JSON written by `scripts/prereq_graph.py`, asks Graphviz
//! (`dot -Tplain`) for a layered layout, and draws everything itself with
//! egui: pan/zoom, search, click a course to highlight what it requires and
//! what it unlocks.
//!
//! Usage: prereq-viewer [graph.json] [--select COURSE] [--check]

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io::Write;
use std::process::{Command, Stdio};

use eframe::egui::{
    self, Align2, Color32, FontId, Key, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Vec2,
};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------- data file

#[derive(Deserialize)]
struct GraphFile {
    rule_source: String,
    courses: BTreeMap<String, Course>,
}

#[derive(Deserialize)]
struct Course {
    title: String,
    #[serde(default)]
    description: String,
    family: String,
    in_chart: bool,
    in_csv: bool,
    in_catalog: bool,
    is_ccs: bool,
    units: String,
    level: String,
    source: String,
    gold_quarter: String,
    catalog_text: String,
    gold_rule: String,
    notes: Vec<String>,
    groups: Vec<Group>,
}

#[derive(Deserialize)]
struct Group {
    alts: Vec<Alt>,
    concurrent: bool,
}

#[derive(Deserialize, Serialize, Clone)]
struct Alt {
    items: Vec<String>,
    concurrent: bool,
}

// ---------------------------------------------------------------- view graph

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Course,
    Exam,
    Wildcard,
    Or,
    And,
}

struct Node {
    key: String, // course id, "exam:..." or junction key
    kind: Kind,
    size: Vec2, // points
    pos: Pos2,  // centre, points, y down
}

struct Edge {
    from: usize,
    to: usize,
    dashed: bool,
    arrow: bool,
    label: Option<String>,
    points: Vec<Pos2>,
}

#[derive(Clone, PartialEq, Debug)]
struct Filters {
    math: bool,
    phys: bool,
    ccs: bool,
    graduate: bool,
    exams: bool,
    hide_unconnected: bool,
}

impl Default for Filters {
    fn default() -> Self {
        Self { math: true, phys: true, ccs: true, graduate: true, exams: true, hide_unconnected: false }
    }
}

#[derive(Default)]
struct View {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    index: HashMap<String, usize>,
    preds: Vec<Vec<usize>>,
    succs: Vec<Vec<usize>>,
}

const PT: f32 = 72.0; // Graphviz works in inches; the view works in points.
const COURSE_W: f32 = 140.0;
const TITLE_CHARS: usize = 26;

fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        match lines.last_mut() {
            Some(l) if l.len() + 1 + word.len() <= width => {
                l.push(' ');
                l.push_str(word);
            }
            _ => lines.push(word.to_string()),
        }
    }
    lines
}

fn is_catalog_marked(c: &Course, gold_in_use: bool) -> bool {
    gold_in_use && c.source != "gold" && !c.groups.is_empty()
}

impl View {
    fn add_node(&mut self, key: &str, kind: Kind, data: &GraphFile) -> usize {
        if let Some(&i) = self.index.get(key) {
            return i;
        }
        let size = match kind {
            Kind::Course => {
                let c = data.courses.get(key);
                let mut lines = c.map_or(0, |c| wrap(&c.title, TITLE_CHARS).len()) as f32;
                if c.is_some_and(|c| is_catalog_marked(c, data.rule_source.contains("GOLD"))) {
                    lines += 1.0;
                }
                if c.is_some_and(|c| !c.in_csv && !c.in_catalog) {
                    lines += 1.0;
                }
                Vec2::new(COURSE_W, 22.0 + 11.0 * lines)
            }
            Kind::Exam | Kind::Wildcard => Vec2::new(110.0, 26.0),
            Kind::Or => Vec2::splat(20.0),
            Kind::And => Vec2::splat(6.0),
        };
        self.index.insert(key.to_string(), self.nodes.len());
        self.nodes.push(Node { key: key.to_string(), kind, size, pos: Pos2::ZERO });
        self.nodes.len() - 1
    }

    fn item_node(&mut self, item: &str, data: &GraphFile) -> usize {
        let kind = if item.starts_with("exam:") {
            Kind::Exam
        } else if item.contains('*') {
            Kind::Wildcard
        } else {
            Kind::Course
        };
        self.add_node(item, kind, data)
    }

    fn edge(&mut self, from: usize, to: usize, dashed: bool, arrow: bool, label: Option<String>) {
        self.edges.push(Edge { from, to, dashed, arrow, label, points: Vec::new() });
    }

    /// Node feeding one alternative (a single item, or an "all of" junction).
    fn alt_source(&mut self, items: &[String], data: &GraphFile) -> usize {
        if items.len() == 1 {
            return self.item_node(&items[0], data);
        }
        let key = format!("and:{}", items.join("|"));
        if let Some(&j) = self.index.get(&key) {
            return j;
        }
        let j = self.add_node(&key, Kind::And, data);
        let mut seen: Vec<&String> = Vec::new();
        for it in items {
            if seen.contains(&it) {
                continue;
            }
            seen.push(it);
            let n = items.iter().filter(|x| *x == it).count();
            let s = self.item_node(it, data);
            self.edge(s, j, false, false, (n > 1).then(|| format!("×{n}")));
        }
        j
    }

    fn build(data: &GraphFile, f: &Filters) -> Self {
        let mut v = View::default();
        let visible: Vec<&String> = data
            .courses
            .iter()
            .filter(|(id, c)| {
                c.in_chart
                    && match c.family.as_str() {
                        "MATH" => f.math,
                        "PHYS" => f.phys,
                        _ => true,
                    }
                    && (f.ccs || !c.is_ccs)
                    && (f.graduate || !matches!(c.level.as_str(), "graduate" | "professional" | "graduate_special_study"))
                    && !id.is_empty()
            })
            .map(|(id, _)| id)
            .collect();
        for id in &visible {
            let c = &data.courses[*id];
            let target = v.add_node(id, Kind::Course, data);
            for g in &c.groups {
                let alts: Vec<Alt> = g
                    .alts
                    .iter()
                    .map(|a| Alt {
                        items: a.items.iter().filter(|i| f.exams || !i.starts_with("exam:")).cloned().collect(),
                        concurrent: a.concurrent,
                    })
                    .filter(|a| !a.items.is_empty())
                    .collect();
                if alts.is_empty() {
                    continue;
                }
                if alts.len() == 1 {
                    let s = v.alt_source(&alts[0].items, data);
                    v.edge(s, target, g.concurrent || alts[0].concurrent, true, None);
                    continue;
                }
                let key = format!("or:{}", serde_json::to_string(&alts).unwrap());
                let j = match v.index.get(&key) {
                    Some(&j) => j,
                    None => {
                        let j = v.add_node(&key, Kind::Or, data);
                        for a in &alts {
                            let s = v.alt_source(&a.items, data);
                            v.edge(s, j, a.concurrent, true, None);
                        }
                        j
                    }
                };
                v.edge(j, target, g.concurrent, true, None);
            }
        }
        if f.hide_unconnected {
            let linked: HashSet<usize> = v.edges.iter().flat_map(|e| [e.from, e.to]).collect();
            let keep: Vec<bool> = (0..v.nodes.len()).map(|i| linked.contains(&i)).collect();
            v.retain(&keep);
        }
        v.adjacency();
        v
    }

    fn retain(&mut self, keep: &[bool]) {
        let mut remap = vec![usize::MAX; self.nodes.len()];
        let mut nodes = Vec::new();
        for (i, n) in std::mem::take(&mut self.nodes).into_iter().enumerate() {
            if keep[i] {
                remap[i] = nodes.len();
                nodes.push(n);
            }
        }
        self.nodes = nodes;
        self.edges.retain(|e| keep[e.from] && keep[e.to]);
        for e in &mut self.edges {
            e.from = remap[e.from];
            e.to = remap[e.to];
        }
        self.index = self.nodes.iter().enumerate().map(|(i, n)| (n.key.clone(), i)).collect();
    }

    fn adjacency(&mut self) {
        self.preds = vec![Vec::new(); self.nodes.len()];
        self.succs = vec![Vec::new(); self.nodes.len()];
        for e in &self.edges {
            self.preds[e.to].push(e.from);
            self.succs[e.from].push(e.to);
        }
    }

    /// Lay out connected nodes with Graphviz; pack unconnected courses in a grid below.
    fn layout(&mut self) -> Result<(), String> {
        let linked: HashSet<usize> = self.edges.iter().flat_map(|e| [e.from, e.to]).collect();
        let mut dot = String::from(
            "digraph G { rankdir=LR; ranksep=0.9; nodesep=0.18; splines=true;\n\
             node [shape=box, fixedsize=true, label=\"\"]; edge [arrowsize=0.7];\n",
        );
        for (i, n) in self.nodes.iter().enumerate() {
            if !linked.contains(&i) {
                continue;
            }
            let shape = match n.kind {
                Kind::Or => "diamond",
                Kind::And => "circle",
                _ => "box",
            };
            dot += &format!(
                "n{i} [width={:.3}, height={:.3}, shape={shape}];\n",
                n.size.x / PT,
                n.size.y / PT
            );
        }
        for e in &self.edges {
            let head = if e.arrow { "normal" } else { "none" };
            dot += &format!("n{} -> n{} [arrowhead={head}];\n", e.from, e.to);
        }
        dot += "}\n";

        let mut child = Command::new("dot")
            .arg("-Tplain")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("could not run Graphviz `dot`: {e}"))?;
        child.stdin.take().unwrap().write_all(dot.as_bytes()).map_err(|e| e.to_string())?;
        let out = child.wait_with_output().map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).into_owned());
        }
        let text = String::from_utf8_lossy(&out.stdout);

        let mut height = 0.0;
        let mut edge_iter = 0;
        for line in text.lines() {
            let t: Vec<&str> = line.split_whitespace().collect();
            let num = |i: usize| t.get(i).and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.0);
            match t.first() {
                Some(&"graph") => height = num(3),
                Some(&"node") => {
                    let i: usize = t[1][1..].parse().map_err(|_| format!("bad node {}", t[1]))?;
                    self.nodes[i].pos = Pos2::new(num(2) * PT, (height - num(3)) * PT);
                }
                Some(&"edge") => {
                    // Edges come back in input order; match on endpoints to be safe.
                    let from: usize = t[1][1..].parse().unwrap_or(usize::MAX);
                    let to: usize = t[2][1..].parse().unwrap_or(usize::MAX);
                    let n: usize = t[3].parse().unwrap_or(0);
                    let pts: Vec<Pos2> = (0..n)
                        .map(|k| Pos2::new(num(4 + 2 * k) * PT, (height - num(5 + 2 * k)) * PT))
                        .collect();
                    let idx = (edge_iter..self.edges.len())
                        .chain(0..edge_iter)
                        .find(|&k| {
                            self.edges[k].from == from && self.edges[k].to == to && self.edges[k].points.is_empty()
                        })
                        .ok_or("edge mismatch in dot output")?;
                    self.edges[idx].points = sample_bezier(&pts);
                    edge_iter = idx + 1;
                }
                _ => {}
            }
        }

        // Grid for unconnected courses, under the laid-out graph.
        let bottom = self
            .nodes
            .iter()
            .enumerate()
            .filter(|(i, _)| linked.contains(i))
            .map(|(_, n)| n.pos.y + n.size.y / 2.0)
            .fold(0.0, f32::max);
        let loose: Vec<usize> = (0..self.nodes.len()).filter(|i| !linked.contains(i)).collect();
        let cols = 8;
        let mut y = bottom + 60.0;
        for row in loose.chunks(cols) {
            let h = row.iter().map(|&i| self.nodes[i].size.y).fold(0.0, f32::max);
            for (c, &i) in row.iter().enumerate() {
                self.nodes[i].pos = Pos2::new(80.0 + c as f32 * (COURSE_W + 20.0), y + h / 2.0);
            }
            y += h + 14.0;
        }
        Ok(())
    }

    fn bounds(&self) -> Rect {
        self.nodes.iter().fold(Rect::NOTHING, |r, n| r.union(Rect::from_center_size(n.pos, n.size)))
    }

    fn reach(&self, start: usize, forward: bool) -> HashSet<usize> {
        let mut seen = HashSet::new();
        let mut q = VecDeque::from([start]);
        while let Some(n) = q.pop_front() {
            for &m in if forward { &self.succs[n] } else { &self.preds[n] } {
                if seen.insert(m) {
                    q.push_back(m);
                }
            }
        }
        seen
    }

    /// Nearest course/exam neighbours, looking through junction nodes.
    fn direct(&self, start: usize, forward: bool) -> Vec<usize> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut q = VecDeque::from([start]);
        while let Some(n) = q.pop_front() {
            for &m in if forward { &self.succs[n] } else { &self.preds[n] } {
                if !seen.insert(m) {
                    continue;
                }
                if matches!(self.nodes[m].kind, Kind::Or | Kind::And) {
                    q.push_back(m);
                } else {
                    out.push(m);
                }
            }
        }
        out.sort_by(|a, b| self.nodes[*a].key.cmp(&self.nodes[*b].key));
        out
    }
}

/// Graphviz gives cubic Bézier control points: p0 (c1 c2 p)*. Sample to a polyline.
fn sample_bezier(p: &[Pos2]) -> Vec<Pos2> {
    if p.len() < 4 {
        return p.to_vec();
    }
    let mut out = vec![p[0]];
    let mut i = 0;
    while i + 3 < p.len() {
        let (a, b, c, d) = (p[i], p[i + 1], p[i + 2], p[i + 3]);
        for k in 1..=10 {
            let t = k as f32 / 10.0;
            let u = 1.0 - t;
            out.push(Pos2::new(
                u * u * u * a.x + 3.0 * u * u * t * b.x + 3.0 * u * t * t * c.x + t * t * t * d.x,
                u * u * u * a.y + 3.0 * u * u * t * b.y + 3.0 * u * t * t * c.y + t * t * t * d.y,
            ));
        }
        i += 3;
    }
    out
}

// ---------------------------------------------------------------- app

struct App {
    data: GraphFile,
    filters: Filters,
    view: View,
    error: Option<String>,
    selected: Option<String>,
    search: String,
    zoom: f32,
    pan: Vec2, // world point shown at the canvas origin
    fit_pending: bool,
    center_pending: bool,
    focus: bool,                   // relayout around the selected course
    laid_out_for: Option<String>, // selection the current layout was built for
}

const MATH_COLORS: (Color32, Color32) = (Color32::from_rgb(0xdb, 0xe8, 0xf7), Color32::from_rgb(0x2b, 0x6c, 0xb0));
const PHYS_COLORS: (Color32, Color32) = (Color32::from_rgb(0xfb, 0xe3, 0xd0), Color32::from_rgb(0xc0, 0x56, 0x21));
const OTHER_COLORS: (Color32, Color32) = (Color32::from_rgb(0xec, 0xec, 0xec), Color32::from_rgb(0x6b, 0x6b, 0x6b));

impl App {
    fn new(data: GraphFile) -> Self {
        let mut app = App {
            data,
            filters: Filters::default(),
            view: View::default(),
            error: None,
            selected: None,
            search: String::new(),
            zoom: 1.0,
            pan: Vec2::ZERO,
            fit_pending: true,
            center_pending: false,
            focus: true,
            laid_out_for: None,
        };
        app.rebuild();
        app
    }

    fn rebuild(&mut self) {
        self.view = View::build(&self.data, &self.filters);
        self.laid_out_for = None;
        if self.focus {
            if let Some(&s) = self.selected.as_ref().and_then(|k| self.view.index.get(k)) {
                // Keep only the selected course's requirement and unlock chains.
                let mut keep = vec![false; self.view.nodes.len()];
                keep[s] = true;
                for i in self.view.reach(s, false).into_iter().chain(self.view.reach(s, true)) {
                    keep[i] = true;
                }
                self.view.retain(&keep);
                self.view.adjacency();
                self.laid_out_for = self.selected.clone();
                self.center_pending = false;
            }
        }
        self.error = self.view.layout().err();
        self.fit_pending = true;
    }

    fn select(&mut self, key: &str) {
        self.selected = Some(key.to_string());
        self.center_pending = true;
    }

    fn colors(&self, n: &Node) -> (Color32, Color32) {
        match self.data.courses.get(&n.key) {
            Some(c) if c.in_chart && c.family == "MATH" => MATH_COLORS,
            Some(c) if c.in_chart && c.family == "PHYS" => PHYS_COLORS,
            _ => OTHER_COLORS,
        }
    }

    fn side_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Prerequisite viewer");
        ui.label(egui::RichText::new(&self.data.rule_source).small().weak());
        ui.separator();

        ui.label("Find a course");
        let resp = ui.text_edit_singleline(&mut self.search);
        let q = self.search.trim().to_lowercase();
        if !q.is_empty() {
            let hits: Vec<String> = self
                .view
                .nodes
                .iter()
                .filter(|n| n.kind == Kind::Course)
                .filter(|n| {
                    n.key.to_lowercase().contains(&q)
                        || self.data.courses.get(&n.key).is_some_and(|c| c.title.to_lowercase().contains(&q))
                })
                .map(|n| n.key.clone())
                .take(12)
                .collect();
            if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                if let Some(h) = hits.first() {
                    let h = h.clone();
                    self.select(&h);
                }
            }
            for h in hits {
                let title = self.data.courses.get(&h).map(|c| c.title.as_str()).unwrap_or("");
                if ui.selectable_label(self.selected.as_ref() == Some(&h), format!("{h}  {title}")).clicked() {
                    self.select(&h);
                }
            }
        }
        ui.separator();

        let before = self.filters.clone();
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.filters.math, "MATH");
            ui.checkbox(&mut self.filters.phys, "PHYS");
            ui.checkbox(&mut self.filters.ccs, "CCS");
        });
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.filters.graduate, "Graduate");
            ui.checkbox(&mut self.filters.exams, "Exams");
        });
        ui.checkbox(&mut self.filters.hide_unconnected, "Hide unconnected courses");
        let focus_before = self.focus;
        ui.checkbox(&mut self.focus, "Focus: re-lay out around the clicked course")
            .on_hover_text("Off: clicking only highlights the course's chain in the full chart");
        if self.filters != before || self.focus != focus_before {
            self.rebuild();
        }
        ui.horizontal(|ui| {
            if ui.button("Fit (F)").clicked() {
                self.fit_pending = true;
            }
            if ui.button("Clear (Esc)").clicked() {
                self.selected = None;
            }
        });
        if let Some(e) = &self.error {
            ui.colored_label(Color32::RED, e);
        }
        ui.separator();

        egui::ScrollArea::vertical().show(ui, |ui| {
            if let Some(key) = self.selected.clone() {
                self.details(ui, &key);
            } else {
                ui.label(egui::RichText::new("Click a course to see what it requires and unlocks.").weak());
            }
            ui.separator();
            legend(ui);
        });
    }

    fn details(&mut self, ui: &mut egui::Ui, key: &str) {
        ui.heading(key.trim_start_matches("exam:"));
        let Some(c) = self.data.courses.get(key) else {
            return;
        };
        if !c.title.is_empty() {
            ui.label(egui::RichText::new(&c.title).strong());
        }
        let mut meta = Vec::new();
        if !c.units.is_empty() {
            meta.push(format!("{} units", c.units));
        }
        if !c.level.is_empty() {
            meta.push(c.level.replace('_', " "));
        }
        if c.is_ccs {
            meta.push("College of Creative Studies".into());
        }
        if !c.in_csv {
            meta.push("not in the CSV export".into());
        }
        if !c.in_catalog {
            meta.push("not in current catalog".into());
        }
        ui.label(egui::RichText::new(meta.join(" · ")).small());
        if !c.description.is_empty() {
            ui.add_space(4.0);
            ui.label(&c.description);
            ui.add_space(4.0);
        }
        if c.in_chart {
            let src = match c.source.as_str() {
                "gold" => format!("GOLD registration rules ({})", c.gold_quarter),
                "catalog" => "catalog text (not listed in GOLD)".into(),
                "description" => "course description (not in catalog)".into(),
                _ => "none".into(),
            };
            ui.label(format!("Rules from: {src}"));
            if !c.gold_rule.is_empty() {
                ui.label(egui::RichText::new("GOLD").strong());
                ui.label(&c.gold_rule);
            }
            if !c.catalog_text.is_empty() {
                ui.label(egui::RichText::new("Catalog").strong());
                ui.label(&c.catalog_text);
            }
            if !c.notes.is_empty() {
                ui.label(egui::RichText::new("Notes").strong());
                for n in &c.notes {
                    ui.label(format!("• {n}"));
                }
            }
        }
        let Some(&i) = self.view.index.get(key) else {
            return;
        };
        let mut clicked = None;
        for (label, forward) in [("Requires (direct)", false), ("Unlocks (direct)", true)] {
            let list = self.view.direct(i, forward);
            if list.is_empty() {
                continue;
            }
            ui.label(egui::RichText::new(label).strong());
            ui.horizontal_wrapped(|ui| {
                for m in list {
                    let k = &self.view.nodes[m].key;
                    if ui.small_button(k.trim_start_matches("exam:")).clicked() {
                        clicked = Some(k.clone());
                    }
                }
            });
        }
        if let Some(k) = clicked {
            self.select(&k);
        }
    }

    fn canvas(&mut self, ui: &mut egui::Ui) {
        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, Color32::WHITE);

        if self.fit_pending && rect.width() > 10.0 {
            let b = self.view.bounds();
            if b.is_positive() {
                self.zoom = (rect.width() / b.width()).min(rect.height() / b.height()) * 0.95;
                self.pan = b.center().to_vec2() - rect.size() / (2.0 * self.zoom);
            }
            self.fit_pending = false;
        }
        if self.center_pending {
            if let Some(&i) = self.selected.as_ref().and_then(|k| self.view.index.get(k)) {
                self.zoom = self.zoom.max(0.9);
                self.pan = self.view.nodes[i].pos.to_vec2() - rect.size() / (2.0 * self.zoom);
            }
            self.center_pending = false;
        }

        // Input: drag to pan, wheel / pinch to zoom around the pointer.
        if resp.dragged() {
            self.pan -= resp.drag_delta() / self.zoom;
        }
        if let Some(ptr) = resp.hover_pos() {
            let (scroll, pinch) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
            let factor = pinch * (scroll * 0.0015).exp();
            if factor != 1.0 {
                let world = self.pan + (ptr - rect.min) / self.zoom;
                self.zoom = (self.zoom * factor).clamp(0.05, 6.0);
                self.pan = world - (ptr - rect.min) / self.zoom;
            }
        }
        let zoom = self.zoom;
        let pan = self.pan;
        let to_screen = |p: Pos2| rect.min + (p.to_vec2() - pan) * zoom;
        let to_world = |p: Pos2| Pos2::ZERO + pan + (p - rect.min) / zoom;

        let hovered = resp.hover_pos().and_then(|p| {
            let w = to_world(p);
            self.view.nodes.iter().position(|n| {
                !matches!(n.kind, Kind::Or | Kind::And) && Rect::from_center_size(n.pos, n.size).contains(w)
            })
        });
        if resp.clicked() {
            match hovered {
                Some(i) => self.selected = Some(self.view.nodes[i].key.clone()),
                None => self.selected = None,
            }
        }

        // Highlight sets.
        let sel = self.selected.as_ref().and_then(|k| self.view.index.get(k)).copied();
        let (anc, desc) = match sel {
            Some(s) => (self.view.reach(s, false), self.view.reach(s, true)),
            None => (HashSet::new(), HashSet::new()),
        };
        let lit = |i: usize| sel.is_none() || Some(i) == sel || anc.contains(&i) || desc.contains(&i);
        let edge_lit = |e: &Edge| {
            sel.is_none()
                || ((anc.contains(&e.from)) && (anc.contains(&e.to) || Some(e.to) == sel))
                || ((Some(e.from) == sel || desc.contains(&e.from)) && desc.contains(&e.to))
        };
        let fade = |c: Color32, on: bool| if on { c } else { c.gamma_multiply(0.15) };

        // Edges.
        for e in &self.view.edges {
            if e.points.len() < 2 {
                continue;
            }
            let on = edge_lit(e);
            let base = if e.dashed { Color32::from_rgb(0x8a, 0x6d, 0x3b) } else { Color32::from_gray(0x55) };
            let highlighted = sel.is_some() && on;
            let color = fade(if highlighted { Color32::from_rgb(0x1f, 0x4e, 0x9a) } else { base }, on);
            let stroke = Stroke::new(if highlighted { 2.0 } else { 1.0 } * zoom.clamp(0.5, 1.5), color);
            let pts: Vec<Pos2> = e.points.iter().map(|&p| to_screen(p)).collect();
            if e.dashed {
                painter.extend(Shape::dashed_line(&pts, stroke, 6.0 * zoom.max(0.3), 4.0 * zoom.max(0.3)));
            } else {
                painter.add(Shape::line(pts.clone(), stroke));
            }
            if e.arrow {
                let tip_base = *pts.last().unwrap();
                let prev = pts[pts.len() - 2];
                let dir = (tip_base - prev).normalized();
                let len = 7.0 * zoom;
                let tip = tip_base + dir * len;
                let side = Vec2::new(-dir.y, dir.x) * len * 0.45;
                painter.add(Shape::convex_polygon(vec![tip, tip_base + side, tip_base - side], color, Stroke::NONE));
            }
            if let Some(l) = &e.label {
                let mid = pts[pts.len() / 2];
                painter.text(mid, Align2::CENTER_BOTTOM, l, FontId::proportional(9.0 * zoom), color);
            }
        }

        // Nodes.
        let gold_in_use = self.data.rule_source.contains("GOLD");
        for (i, n) in self.view.nodes.iter().enumerate() {
            let on = lit(i);
            let r = Rect::from_center_size(to_screen(n.pos), n.size * zoom);
            if !r.intersects(rect) {
                continue;
            }
            match n.kind {
                Kind::Or => {
                    let c = r.center();
                    let h = r.width() / 2.0;
                    let poly = vec![c - Vec2::Y * h, c + Vec2::X * h, c + Vec2::Y * h, c - Vec2::X * h];
                    painter.add(Shape::convex_polygon(
                        poly,
                        fade(Color32::from_rgb(0xff, 0xf6, 0xd6), on),
                        Stroke::new(1.0, fade(Color32::from_rgb(0xb7, 0x95, 0x0b), on)),
                    ));
                    if zoom > 0.5 {
                        painter.text(c, Align2::CENTER_CENTER, "or", FontId::proportional(7.0 * zoom),
                                     fade(Color32::from_gray(0x55), on));
                    }
                }
                Kind::And => {
                    painter.circle_filled(r.center(), (3.0 * zoom).max(1.5), fade(Color32::from_gray(0x33), on));
                }
                Kind::Exam | Kind::Wildcard => {
                    let label = if n.kind == Kind::Exam {
                        n.key.trim_start_matches("exam:").to_string()
                    } else {
                        let (s, num) = n.key.rsplit_once(' ').unwrap_or(("", &n.key));
                        format!("any {s} {} course", num.replace('*', "xx"))
                    };
                    painter.rect(r, 2.0, fade(Color32::from_gray(0xf4), on),
                                 Stroke::new(1.0, fade(Color32::from_gray(0x99), on)), StrokeKind::Inside);
                    if zoom > 0.3 {
                        painter.text(r.center(), Align2::CENTER_CENTER, label, FontId::proportional(8.0 * zoom),
                                     fade(Color32::from_gray(0x33), on));
                    }
                }
                Kind::Course => {
                    let c = self.data.courses.get(&n.key);
                    let (fill, border) = self.colors(n);
                    let ccs = c.is_some_and(|c| c.is_ccs);
                    let width = if Some(i) == sel { 3.0 } else if ccs { 2.2 } else { 1.0 };
                    let stroke = Stroke::new(width, fade(border, on));
                    let radius = 6.0 * zoom;
                    if c.is_some_and(|c| c.in_csv) {
                        painter.rect(r, radius, fade(fill, on), stroke, StrokeKind::Inside);
                    } else {
                        painter.rect_filled(r, radius, fade(fill, on));
                        let pts = vec![r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()];
                        painter.extend(Shape::dashed_line(&pts, stroke, 3.0, 3.0));
                    }
                    if zoom < 0.18 {
                        continue;
                    }
                    let ink = fade(Color32::from_gray(0x11), on);
                    let mut y = r.top() + 4.0 * zoom;
                    painter.text(Pos2::new(r.center().x, y), Align2::CENTER_TOP, &n.key,
                                 FontId::proportional(11.0 * zoom), ink);
                    y += 14.0 * zoom;
                    let Some(c) = c else { continue };
                    if zoom < 0.4 {
                        continue;
                    }
                    for line in wrap(&c.title, TITLE_CHARS) {
                        painter.text(Pos2::new(r.center().x, y), Align2::CENTER_TOP, line,
                                     FontId::proportional(8.5 * zoom), ink);
                        y += 11.0 * zoom;
                    }
                    let grey = fade(Color32::from_gray(0x88), on);
                    if !c.in_csv && !c.in_catalog {
                        painter.text(Pos2::new(r.center().x, y), Align2::CENTER_TOP, "not in current catalog",
                                     FontId::proportional(7.5 * zoom), grey);
                        y += 11.0 * zoom;
                    }
                    if is_catalog_marked(c, gold_in_use) {
                        painter.text(Pos2::new(r.center().x, y), Align2::CENTER_TOP, "catalog rules (not in GOLD)",
                                     FontId::proportional(7.5 * zoom), grey);
                    }
                }
            }
        }

        if let Some(i) = hovered {
            let n = &self.view.nodes[i];
            if let Some(c) = self.data.courses.get(&n.key) {
                resp.on_hover_text(format!("{} — {}", n.key, c.title));
            }
        }
    }
}

fn legend(ui: &mut egui::Ui) {
    ui.label(egui::RichText::new("Legend").strong());
    for line in [
        "solid arrow: must be completed first",
        "dashed arrow: may be taken concurrently",
        "yellow diamond (or): choose one",
        "black dot: all of these",
        "blue / orange: MATH / PHYS · grey: other departments",
        "thick border: College of Creative Studies",
        "dashed border: not in the CSV export",
        "drag to pan · scroll to zoom",
    ] {
        ui.label(egui::RichText::new(line).small());
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
    }
}

impl App {
    fn draw(&mut self, ui: &mut egui::Ui) {
        if self.focus && self.selected != self.laid_out_for {
            self.rebuild();
        }
        if !ui.ctx().egui_wants_keyboard_input() {
            ui.input(|i| {
                if i.key_pressed(Key::F) {
                    self.fit_pending = true;
                }
                if i.key_pressed(Key::Escape) {
                    self.selected = None;
                }
            });
        }
        egui::Panel::right("side").default_size(320.0).show(ui, |ui| self.side_panel(ui));
        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| self.canvas(ui));
    }
}

// ---------------------------------------------------------------- main

fn check(data: &GraphFile) -> Result<(), String> {
    let charted = data.courses.values().filter(|c| c.in_chart).count();
    for (name, f) in [
        ("all", Filters::default()),
        ("PHYS only", Filters { math: false, ..Filters::default() }),
        ("no exams, hide unconnected", Filters { exams: false, hide_unconnected: true, ..Filters::default() }),
    ] {
        let mut v = View::build(data, &f);
        v.layout()?;
        let courses = v.nodes.iter().filter(|n| n.kind == Kind::Course).count();
        let shown = v.nodes.iter().filter(|n| data.courses.get(&n.key).is_some_and(|c| c.in_chart)).count();
        if name == "all" && shown != charted {
            return Err(format!("expected {charted} charted courses, found {shown}"));
        }
        if let Some(e) = v.edges.iter().find(|e| e.points.len() < 2) {
            return Err(format!("edge {} -> {} has no route", v.nodes[e.from].key, v.nodes[e.to].key));
        }
        let boxes: Vec<(usize, Rect)> = v
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| !matches!(n.kind, Kind::Or | Kind::And))
            .map(|(i, n)| (i, Rect::from_center_size(n.pos, n.size).shrink(0.5)))
            .collect();
        for (a, (i, ra)) in boxes.iter().enumerate() {
            for (j, rb) in &boxes[a + 1..] {
                if ra.intersects(*rb) {
                    return Err(format!("{} overlaps {}", v.nodes[*i].key, v.nodes[*j].key));
                }
            }
        }
        println!(
            "{name}: {} nodes ({courses} course boxes), {} edges, bounds {:.0}x{:.0} pt - ok",
            v.nodes.len(),
            v.edges.len(),
            v.bounds().width(),
            v.bounds().height()
        );
    }
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let select = args.iter().position(|a| a == "--select").and_then(|i| args.get(i + 1)).cloned();
    let path = args
        .iter()
        .enumerate()
        .find(|(i, a)| !a.starts_with("--") && (*i == 0 || args[i - 1] != "--select"))
        .map(|(_, a)| a.clone())
        .unwrap_or_else(|| "charts/prereq-graph.json".into());
    let data: GraphFile = match std::fs::read_to_string(&path)
        .map_err(|e| e.to_string())
        .and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
    {
        Ok(d) => d,
        Err(e) => {
            eprintln!("could not load {path}: {e}");
            std::process::exit(1);
        }
    };
    if args.iter().any(|a| a == "--check") {
        if let Err(e) = check(&data) {
            eprintln!("check failed: {e}");
            std::process::exit(1);
        }
        return;
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title("Prerequisite viewer").with_inner_size([1400.0, 900.0]),
        ..Default::default()
    };
    if let Err(e) = eframe::run_native("prereq-viewer", options, Box::new(|_cc| {
        let mut app = App::new(data);
        if let Some(s) = select {
            app.select(&s);
        }
        Ok(Box::new(app))
    })) {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{Harness, kittest::Queryable};

    fn app() -> App {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../charts/prereq-graph.json");
        App::new(serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap())
    }

    fn snapshot(app: &App) -> (usize, Vec<(String, Pos2)>, f32) {
        let mut pos: Vec<(String, Pos2)> = app.view.nodes.iter().map(|n| (n.key.clone(), n.pos)).collect();
        pos.sort_by(|a, b| a.0.cmp(&b.0));
        (app.view.nodes.len(), pos, app.zoom)
    }

    #[test]
    fn filter_checkboxes_relayout() {
        let mut h = Harness::builder().with_size(Vec2::new(1400.0, 900.0)).build_ui_state(|ui, app: &mut App| app.draw(ui), app());
        h.run();
        let before = snapshot(h.state());
        for label in ["MATH", "Hide unconnected courses", "Exams"] {
            h.get_by_label(label).click();
            h.run();
            let after = snapshot(h.state());
            eprintln!("{label}: {} -> {} nodes, zoom {:.3} -> {:.3}, filters {:?}",
                      before.0, after.0, before.2, after.2, h.state().filters.math);
            assert_ne!(before.1, after.1, "layout unchanged after toggling {label}");
            h.get_by_label(label).click();
            h.run();
            assert_eq!(snapshot(h.state()).0, before.0, "toggling {label} back did not restore");
        }
    }

    #[test]
    fn selecting_relayouts_in_focus_mode() {
        let mut h = Harness::builder().with_size(Vec2::new(1400.0, 900.0)).build_ui_state(|ui, app: &mut App| app.draw(ui), app());
        h.run();
        let full = h.state().view.nodes.len();
        h.state_mut().select("PHYS 115A");
        h.run();
        let focused = h.state().view.nodes.len();
        eprintln!("full {full} nodes -> focused {focused}");
        assert!(focused < full / 3, "focus view should be much smaller");
        assert!(h.state().view.index.contains_key("PHYS 100A") && h.state().view.index.contains_key("PHYS 115B"));
        h.state_mut().selected = None;
        h.run();
        assert_eq!(h.state().view.nodes.len(), full, "clearing selection restores the full chart");
    }
}
