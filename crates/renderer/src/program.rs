//! The program behind the call graph: every loaded object merged and
//! regrouped by source file, class and function, so the Visualizer can show
//! it a level at a time.
//!
//! - [`Level::Files`]: the source files, joined by `#include`.
//! - [`Level::File`]: one file's classes, functions and globals, with the
//!   calls between them and the functions in other files they call or are
//!   called by.
//! - [`Level::Class`]: a class's member functions and data members.
//! - [`Level::Function`]: a function's parameters and locals.
//!
//! Merging works the way a linker would, by linkage name: an `External` in
//! one object becomes the definition from another. A definition compiled
//! into several objects (an inline function in a header) is one function
//! when it comes from the same source file; two `static` functions with
//! one name in different files stay apart.
//!
//! [`Program::build`] reads the source files from disk (to follow
//! `#include` lines and count lines); everything after it is pure.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use engine::{CodeGraph, EdgeKind, NodeId, NodeKind, SourceLoc};

use crate::model::{EdgeClass, GraphModel, ModelEdge, ModelNode, NodeClass};

/// Which optional entities to show. The default is project code only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Filter {
    /// Standard-library and toolchain code, and the headers it lives in.
    pub library: bool,
    /// Compiler-generated symbols (RTTI, vtables, literals, ...).
    pub generated: bool,
    /// Calls into code no loaded object defines (`printf`, iostream
    /// operators, runtime helpers).
    pub external: bool,
}

/// How much of the program the call graph shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GraphMode {
    /// Only the functions, classes and globals the user wrote.
    #[default]
    Abstracted,
    /// Everything: library code, compiler-generated symbols and calls into
    /// code outside the loaded objects.
    Verbose,
}

impl GraphMode {
    pub const ALL: [Self; 2] = [Self::Abstracted, Self::Verbose];

    pub fn filter(self) -> Filter {
        let all = self == Self::Verbose;
        Filter {
            library: all,
            generated: all,
            external: all,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Abstracted => "Abstracted",
            Self::Verbose => "Verbose",
        }
    }
}

/// One thing in the program, by index into its [`Program`] table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Entity {
    File(u32),
    Class(u32),
    Function(u32),
    Global(u32),
    Field(u32),
    Local(u32),
    External(u32),
}

/// What the call graph shows. Levels name their subject by stable id, so
/// they survive a reload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Level {
    #[default]
    Files,
    File(NodeId),
    Class(NodeId),
    Function(NodeId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Loc {
    pub file: u32,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub struct FileInfo {
    pub id: NodeId,
    /// Resolved on disk when `exists`, else as debug info spelled it.
    pub path: PathBuf,
    pub name: Arc<str>,
    pub exists: bool,
    pub library: bool,
    pub lines: Option<usize>,
    pub bytes: Option<u64>,
    pub includes: Vec<u32>,
    pub included_by: Vec<u32>,
    /// `#include` targets not found in the project (system headers).
    pub external_includes: Vec<String>,
    /// Objects whose code or debug info comes from this file.
    pub objects: BTreeSet<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ClassInfo {
    pub id: NodeId,
    pub name: Arc<str>,
    pub loc: Option<Loc>,
    pub size: Option<u64>,
    pub fields: Vec<u32>,
    pub methods: Vec<u32>,
    pub library: bool,
}

#[derive(Debug, Clone)]
pub struct FunctionInfo {
    pub id: NodeId,
    pub name: Arc<str>,
    pub linkage: Arc<str>,
    pub loc: Option<Loc>,
    pub size: Option<u64>,
    pub object: PathBuf,
    pub class: Option<u32>,
    pub locals: Vec<u32>,
    pub library: bool,
    pub artificial: bool,
}

#[derive(Debug, Clone)]
pub struct GlobalInfo {
    pub id: NodeId,
    pub name: Arc<str>,
    pub linkage: Arc<str>,
    pub loc: Option<Loc>,
    pub size: Option<u64>,
    pub object: PathBuf,
    pub ty: Option<Arc<str>>,
    pub library: bool,
    pub artificial: bool,
}

/// A data member of a class, or a function's parameter or local.
#[derive(Debug, Clone)]
pub struct MemberInfo {
    pub id: NodeId,
    pub name: Arc<str>,
    /// The class (fields) or function (locals) it belongs to.
    pub owner: u32,
    pub ty: Option<Arc<str>>,
    pub loc: Option<Loc>,
}

/// Referenced but defined in no loaded object.
#[derive(Debug, Clone)]
pub struct ExternalInfo {
    pub id: NodeId,
    pub name: Arc<str>,
    pub linkage: Arc<str>,
    /// A compiler-inserted call such as MinGW's `__main`.
    pub artificial: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Link {
    from: Entity,
    to: Entity,
    class: EdgeClass,
}

/// The modal's content for one entity.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Details {
    pub title: String,
    /// "class · 16 bytes" and the like.
    pub kind: String,
    pub rows: Vec<(String, String)>,
    /// Headed lists (members, callers, includes, ...); empty ones omitted.
    pub lists: Vec<(String, Vec<String>)>,
    /// Where to open the source, when the file is on disk.
    pub source: Option<(PathBuf, u32)>,
}

#[derive(Debug, Default)]
pub struct Program {
    pub files: Vec<FileInfo>,
    pub classes: Vec<ClassInfo>,
    pub functions: Vec<FunctionInfo>,
    pub globals: Vec<GlobalInfo>,
    pub fields: Vec<MemberInfo>,
    pub locals: Vec<MemberInfo>,
    pub externals: Vec<ExternalInfo>,
    links: Vec<Link>,
    by_id: HashMap<NodeId, Entity>,
}

/// Source extensions whose `#include` lines are followed.
const C_FAMILY: &[&str] = &[
    "c", "cc", "cpp", "cxx", "c++", "h", "hh", "hpp", "hxx", "h++", "inl", "ipp", "tpp", "ixx",
];
/// Files larger than this are not read (generated amalgamations).
const MAX_SCAN_BYTES: u64 = 8 << 20;
/// Bounds the include walk on huge trees.
const MAX_FILES: usize = 20_000;

impl Program {
    /// Merges `graphs` (any order; the result is deterministic). `root` is
    /// the open folder, used to find sources and included headers.
    pub fn build(graphs: &[Arc<CodeGraph>], root: Option<&Path>) -> Self {
        let mut graphs: Vec<&CodeGraph> = graphs.iter().map(|g| &**g).collect();
        graphs.sort_by(|a, b| a.path.cmp(&b.path));
        let mut b = Builder {
            p: Program::default(),
            root: root.map(Path::to_path_buf),
            file_keys: HashMap::new(),
            classes_by_name: HashMap::new(),
            resolved: HashMap::new(),
        };
        let canon: Vec<HashMap<NodeId, Entity>> = graphs.iter().map(|g| b.definitions(g)).collect();

        // Externals resolve to the first definition of their linkage name.
        let mut defined: HashMap<Arc<str>, Entity> = HashMap::new();
        for (i, f) in b.p.functions.iter().enumerate() {
            defined
                .entry(f.linkage.clone())
                .or_insert(Entity::Function(i as u32));
        }
        for (i, g) in b.p.globals.iter().enumerate() {
            defined
                .entry(g.linkage.clone())
                .or_insert(Entity::Global(i as u32));
        }
        let mut externals: HashMap<Arc<str>, u32> = HashMap::new();
        let mut links = HashSet::new();
        for (g, mut canon) in graphs.iter().zip(canon) {
            for n in g.nodes_of(NodeKind::External) {
                let e = match defined.get(&n.linkage_name) {
                    Some(&e) => e,
                    None => {
                        let next = b.p.externals.len() as u32;
                        let i = *externals.entry(n.linkage_name.clone()).or_insert(next);
                        if i == next {
                            b.p.externals.push(ExternalInfo {
                                id: n.id,
                                name: n.display_name.clone(),
                                linkage: n.linkage_name.clone(),
                                artificial: n.artificial,
                            });
                        }
                        Entity::External(i)
                    }
                };
                canon.insert(n.id, e);
            }
            for e in g.edges() {
                let class = match e.kind {
                    EdgeKind::Calls => EdgeClass::Calls,
                    EdgeKind::References => EdgeClass::References,
                    _ => continue,
                };
                if let (Some(&from), Some(&to)) = (canon.get(&e.from), canon.get(&e.to)) {
                    let symbol = |e: Entity| {
                        matches!(
                            e,
                            Entity::Function(_) | Entity::Global(_) | Entity::External(_)
                        )
                    };
                    if from != to && symbol(from) && symbol(to) {
                        links.insert(Link { from, to, class });
                    }
                }
            }
        }
        let mut links: Vec<Link> = links.into_iter().collect();
        links.sort_by_key(|l| (key(l.from), key(l.to), l.class));
        b.p.links = links;
        b.scan_includes();

        let mut p = b.p;
        let mut by_id = HashMap::new();
        let mut index = |ids: Vec<NodeId>, make: fn(u32) -> Entity| {
            for (i, id) in ids.into_iter().enumerate() {
                by_id.entry(id).or_insert(make(i as u32));
            }
        };
        index(p.files.iter().map(|x| x.id).collect(), Entity::File);
        index(p.classes.iter().map(|x| x.id).collect(), Entity::Class);
        index(p.functions.iter().map(|x| x.id).collect(), Entity::Function);
        index(p.globals.iter().map(|x| x.id).collect(), Entity::Global);
        index(p.fields.iter().map(|x| x.id).collect(), Entity::Field);
        index(p.locals.iter().map(|x| x.id).collect(), Entity::Local);
        index(p.externals.iter().map(|x| x.id).collect(), Entity::External);
        p.by_id = by_id;
        p
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
            && self.functions.is_empty()
            && self.globals.is_empty()
            && self.externals.is_empty()
    }

    pub fn entity(&self, id: NodeId) -> Option<Entity> {
        self.by_id.get(&id).copied()
    }

    pub fn id_of(&self, e: Entity) -> Option<NodeId> {
        Some(match e {
            Entity::File(i) => self.files.get(i as usize)?.id,
            Entity::Class(i) => self.classes.get(i as usize)?.id,
            Entity::Function(i) => self.functions.get(i as usize)?.id,
            Entity::Global(i) => self.globals.get(i as usize)?.id,
            Entity::Field(i) => self.fields.get(i as usize)?.id,
            Entity::Local(i) => self.locals.get(i as usize)?.id,
            Entity::External(i) => self.externals.get(i as usize)?.id,
        })
    }

    /// The level that shows what `e` contains, if it has one.
    pub fn level_of(&self, e: Entity) -> Option<Level> {
        let id = self.id_of(e)?;
        match e {
            Entity::File(_) => Some(Level::File(id)),
            Entity::Class(_) => Some(Level::Class(id)),
            Entity::Function(_) => Some(Level::Function(id)),
            _ => None,
        }
    }

    /// Whether `level` still names something (after a reload).
    pub fn has_level(&self, level: Level) -> bool {
        match level {
            Level::Files => true,
            Level::File(id) => matches!(self.entity(id), Some(Entity::File(_))),
            Level::Class(id) => matches!(self.entity(id), Some(Entity::Class(_))),
            Level::Function(id) => matches!(self.entity(id), Some(Entity::Function(_))),
        }
    }

    /// The level above: a class or function's file, or the file list.
    pub fn parent(&self, level: Level) -> Option<Level> {
        let file = |loc: Option<Loc>| {
            loc.and_then(|l| self.files.get(l.file as usize))
                .map_or(Level::Files, |f| Level::File(f.id))
        };
        match (level, self.subject(level)) {
            (Level::Files, _) => None,
            (_, Some(Entity::Class(c))) => Some(file(self.classes[c as usize].loc)),
            (_, Some(Entity::Function(f))) => Some(file(self.functions[f as usize].loc)),
            _ => Some(Level::Files),
        }
    }

    /// Names along the way to `level`, starting at the file list.
    pub fn breadcrumb(&self, level: Level) -> Vec<(String, Level)> {
        let mut out = Vec::new();
        let mut at = Some(level);
        while let Some(l) = at {
            let name = match self.subject(l) {
                None => "Files".to_string(),
                Some(e) => self.name(e),
            };
            out.push((name, l));
            at = self.parent(l);
        }
        out.reverse();
        out
    }

    fn subject(&self, level: Level) -> Option<Entity> {
        match level {
            Level::Files => None,
            Level::File(id) | Level::Class(id) | Level::Function(id) => self.entity(id),
        }
    }

    /// A short display name.
    pub fn name(&self, e: Entity) -> String {
        match e {
            Entity::File(i) => self.files[i as usize].name.to_string(),
            Entity::Class(i) => self.classes[i as usize].name.to_string(),
            Entity::Function(i) => short_name(&self.functions[i as usize].name),
            Entity::Global(i) => short_name(&self.globals[i as usize].name),
            Entity::Field(i) => self.fields[i as usize].name.to_string(),
            Entity::Local(i) => self.locals[i as usize].name.to_string(),
            Entity::External(i) => short_name(&self.externals[i as usize].name),
        }
    }

    fn visible(&self, e: Entity, f: Filter) -> bool {
        let ok = |library: bool, artificial: bool| {
            (!library || f.library) && (!artificial || f.generated)
        };
        match e {
            Entity::File(i) => ok(self.files[i as usize].library, false),
            Entity::Class(i) => ok(self.classes[i as usize].library, false),
            Entity::Function(i) => {
                let x = &self.functions[i as usize];
                ok(x.library, x.artificial)
            }
            Entity::Global(i) => {
                let x = &self.globals[i as usize];
                ok(x.library, x.artificial)
            }
            Entity::External(i) => f.external && ok(false, self.externals[i as usize].artificial),
            Entity::Field(_) | Entity::Local(_) => true,
        }
    }

    fn class_of(&self, e: Entity) -> NodeClass {
        let pick = |library: bool, artificial: bool, own: NodeClass| {
            if artificial {
                NodeClass::Generated
            } else if library {
                NodeClass::Library
            } else {
                own
            }
        };
        match e {
            Entity::File(i) => pick(self.files[i as usize].library, false, NodeClass::File),
            Entity::Class(i) => pick(self.classes[i as usize].library, false, NodeClass::Class),
            Entity::Function(i) => {
                let x = &self.functions[i as usize];
                pick(x.library, x.artificial, NodeClass::Function)
            }
            Entity::Global(i) => {
                let x = &self.globals[i as usize];
                pick(x.library, x.artificial, NodeClass::Variable)
            }
            Entity::Field(_) => NodeClass::Field,
            Entity::Local(_) => NodeClass::Local,
            Entity::External(_) => NodeClass::External,
        }
    }

    fn file_of(&self, e: Entity) -> Option<u32> {
        let loc = match e {
            Entity::File(i) => return Some(i),
            Entity::Class(i) => self.classes[i as usize].loc,
            Entity::Function(i) => self.functions[i as usize].loc,
            Entity::Global(i) => self.globals[i as usize].loc,
            Entity::Field(i) => self.fields[i as usize].loc,
            Entity::Local(i) => self.locals[i as usize].loc,
            Entity::External(_) => None,
        };
        loc.map(|l| l.file)
    }

    /// The graph for one level.
    pub fn model(&self, level: Level, filter: Filter) -> GraphModel {
        let mut m = ModelBuilder::default();
        match (level, self.subject(level)) {
            (Level::Files, _) => {
                for i in 0..self.files.len() as u32 {
                    m.add_or_hide(self, Entity::File(i), filter, None);
                }
                for (i, f) in self.files.iter().enumerate() {
                    for &to in &f.includes {
                        m.link(
                            Entity::File(i as u32),
                            Entity::File(to),
                            EdgeClass::Includes,
                        );
                    }
                }
            }
            (Level::File(_), Some(Entity::File(file))) => self.file_level(file, filter, &mut m),
            (Level::Class(_), Some(Entity::Class(c))) => {
                let class = &self.classes[c as usize];
                let hub = m.add(self, Entity::Class(c), None);
                m.hub = Some(hub);
                for &f in &class.methods {
                    let label = method_label(&self.name(Entity::Function(f)), &class.name);
                    m.add_or_hide(self, Entity::Function(f), filter, Some(label));
                }
                for &f in &class.fields {
                    m.add(self, Entity::Field(f), None);
                }
                m.spokes();
                for l in &self.links {
                    if m.contains(l.from) && m.contains(l.to) {
                        m.link(l.from, l.to, l.class);
                    }
                }
            }
            (Level::Function(_), Some(Entity::Function(f))) => {
                let hub = m.add(self, Entity::Function(f), None);
                m.hub = Some(hub);
                for &l in &self.functions[f as usize].locals {
                    m.add(self, Entity::Local(l), None);
                }
                m.spokes();
            }
            _ => {}
        }
        m.finish()
    }

    /// A file's classes, free functions and globals around the file itself;
    /// calls to and from other files end at faded outside nodes.
    fn file_level(&self, file: u32, filter: Filter, m: &mut ModelBuilder) {
        let hub = m.add(self, Entity::File(file), None);
        m.hub = Some(hub);
        let here = |loc: Option<Loc>| loc.is_some_and(|l| l.file == file);
        for (i, c) in self.classes.iter().enumerate() {
            let methods_here = c
                .methods
                .iter()
                .any(|&f| here(self.functions[f as usize].loc));
            if here(c.loc) || methods_here {
                m.add_or_hide(self, Entity::Class(i as u32), filter, None);
            }
        }
        for (i, f) in self.functions.iter().enumerate() {
            if f.class.is_none() && here(f.loc) {
                m.add_or_hide(self, Entity::Function(i as u32), filter, None);
            }
        }
        for (i, g) in self.globals.iter().enumerate() {
            if here(g.loc) {
                m.add_or_hide(self, Entity::Global(i as u32), filter, None);
            }
        }
        m.spokes();
        // A method stands for its class when the class is shown here.
        let inside = |m: &ModelBuilder, e: Entity| match e {
            Entity::Function(f) => match self.functions[f as usize].class {
                Some(c) if m.contains(Entity::Class(c)) => Some(Entity::Class(c)),
                _ => m.contains(e).then_some(e),
            },
            _ => m.contains(e).then_some(e),
        };
        for l in &self.links {
            let (a, b) = (inside(m, l.from), inside(m, l.to));
            if a.is_none() && b.is_none() {
                continue;
            }
            let end = |m: &mut ModelBuilder, e: Entity, placed: Option<Entity>| match placed {
                Some(p) => Some(p),
                None if self.visible(e, filter) => {
                    m.add_outside(self, e);
                    Some(e)
                }
                None => None,
            };
            if let (Some(from), Some(to)) = (end(m, l.from, a), end(m, l.to, b)) {
                m.link(from, to, l.class);
            }
        }
    }

    /// Where to open `e`'s source, when the file is on disk.
    pub fn source(&self, e: Entity) -> Option<(PathBuf, u32)> {
        let (file, line) = match e {
            Entity::File(i) => (i, 1),
            _ => {
                let loc = match e {
                    Entity::Class(i) => self.classes[i as usize].loc,
                    Entity::Function(i) => self.functions[i as usize].loc,
                    Entity::Global(i) => self.globals[i as usize].loc,
                    Entity::Field(i) => self.fields[i as usize].loc,
                    Entity::Local(i) => self.locals[i as usize].loc,
                    _ => None,
                }
                .or_else(|| match e {
                    // A data member without its own line: its class's.
                    Entity::Field(i) => self.classes[self.fields[i as usize].owner as usize].loc,
                    _ => None,
                })?;
                (loc.file, loc.line)
            }
        };
        let f = self.files.get(file as usize)?;
        f.exists.then(|| (f.path.clone(), line.max(1)))
    }

    fn at(&self, loc: Option<Loc>) -> Option<String> {
        let l = loc?;
        let name = &self.files.get(l.file as usize)?.name;
        Some(if l.line > 0 {
            format!("{name}:{}", l.line)
        } else {
            name.to_string()
        })
    }

    fn linked(&self, e: Entity, outgoing: bool, class: EdgeClass) -> Vec<String> {
        let mut v: Vec<String> = self
            .links
            .iter()
            .filter(|l| l.class == class)
            .filter_map(|l| match outgoing {
                true if l.from == e => Some(l.to),
                false if l.to == e => Some(l.from),
                _ => None,
            })
            // Vtables, initialisers and the like are noise here.
            .filter(|&o| {
                let library = Filter {
                    library: true,
                    generated: false,
                    external: true,
                };
                self.visible(o, library)
            })
            .map(|o| self.qualified_name(o))
            .collect();
        v.sort();
        v.dedup();
        v
    }

    /// Name with the file it lives in, for lists that cross files.
    fn qualified_name(&self, e: Entity) -> String {
        match self.file_of(e).and_then(|f| self.files.get(f as usize)) {
            Some(f) => format!("{}  ({})", self.name(e), f.name),
            None => self.name(e),
        }
    }

    /// Everything the modal shows for `e`.
    pub fn details(&self, e: Entity) -> Details {
        let mut d = Details {
            title: self.name(e),
            source: self.source(e),
            ..Details::default()
        };
        let mut row = |k: &str, v: String| d.rows.push((k.to_string(), v));
        let size = |s: Option<u64>| s.map(|s| format!("{s} bytes"));
        let mut lists: Vec<(String, Vec<String>)> = Vec::new();
        match e {
            Entity::File(i) => {
                let f = &self.files[i as usize];
                d.kind = if f.library {
                    "library header".into()
                } else {
                    "source file".into()
                };
                row("Path", f.path.display().to_string());
                if let Some(n) = f.lines {
                    row("Lines", n.to_string());
                }
                if let Some(n) = f.bytes {
                    row("Size", format!("{n} bytes"));
                }
                if !f.exists {
                    row("On disk", "not found".into());
                }
                let objects: Vec<String> =
                    f.objects.iter().map(|o| file_name(o).to_string()).collect();
                let names = |v: &[u32]| -> Vec<String> {
                    v.iter()
                        .map(|&j| self.files[j as usize].name.to_string())
                        .collect()
                };
                let mut includes = names(&f.includes);
                includes.extend(f.external_includes.iter().map(|s| format!("<{s}>")));
                lists.push(("Includes".into(), includes));
                lists.push(("Included by".into(), names(&f.included_by)));
                let here_e = |e: Entity| self.file_of(e) == Some(i);
                let user = Filter {
                    library: true,
                    generated: false,
                    external: true,
                };
                let sorted = |es: Vec<Entity>| -> Vec<String> {
                    let mut v: Vec<String> = es
                        .into_iter()
                        .filter(|&e| self.visible(e, user) && here_e(e))
                        .map(|e| self.name(e))
                        .collect();
                    v.sort();
                    v
                };
                let classes = sorted((0..self.classes.len() as u32).map(Entity::Class).collect());
                let functions = sorted(
                    (0..self.functions.len() as u32)
                        .map(Entity::Function)
                        .collect(),
                );
                let globals = sorted((0..self.globals.len() as u32).map(Entity::Global).collect());
                lists.push(("Classes".into(), classes));
                lists.push(("Functions".into(), functions));
                lists.push(("Globals".into(), globals));
                lists.push(("Compiled into".into(), objects));
            }
            Entity::Class(i) => {
                let c = &self.classes[i as usize];
                d.kind = join_kind("class", size(c.size));
                if let Some(at) = self.at(c.loc) {
                    row("Declared at", at);
                }
                if c.library {
                    row("Library", "yes".into());
                }
                let fields = c
                    .fields
                    .iter()
                    .map(|&f| typed(&self.fields[f as usize]))
                    .collect();
                let methods = c
                    .methods
                    .iter()
                    .map(|&f| self.functions[f as usize].name.to_string())
                    .collect();
                lists.push(("Data members".into(), fields));
                lists.push(("Member functions".into(), methods));
            }
            Entity::Function(i) => {
                let f = &self.functions[i as usize];
                d.title = f.name.to_string();
                let kind = match f.class {
                    Some(c) => format!("member function of {}", self.classes[c as usize].name),
                    None => "function".into(),
                };
                d.kind = join_kind(&kind, size(f.size).map(|s| format!("{s} of code")));
                if let Some(at) = self.at(f.loc) {
                    row("Defined at", at);
                }
                row("Linkage name", f.linkage.to_string());
                row("Object", file_name(&f.object).to_string());
                if f.artificial {
                    row("Compiler-generated", "yes".into());
                } else if f.library {
                    row("Library", "yes".into());
                }
                let locals = f
                    .locals
                    .iter()
                    .map(|&l| typed(&self.locals[l as usize]))
                    .collect();
                lists.push(("Parameters and locals".into(), locals));
                lists.push(("Calls".into(), self.linked(e, true, EdgeClass::Calls)));
                lists.push(("Called by".into(), self.linked(e, false, EdgeClass::Calls)));
                lists.push(("Uses".into(), self.linked(e, true, EdgeClass::References)));
                lists.push((
                    "Used by".into(),
                    self.linked(e, false, EdgeClass::References),
                ));
            }
            Entity::Global(i) => {
                let g = &self.globals[i as usize];
                d.title = g.name.to_string();
                d.kind = join_kind("global variable", size(g.size));
                if let Some(t) = &g.ty {
                    row("Type", t.to_string());
                }
                if let Some(at) = self.at(g.loc) {
                    row("Declared at", at);
                }
                row("Linkage name", g.linkage.to_string());
                row("Object", file_name(&g.object).to_string());
                lists.push((
                    "Used by".into(),
                    self.linked(e, false, EdgeClass::References),
                ));
                lists.push(("Uses".into(), self.linked(e, true, EdgeClass::References)));
            }
            Entity::Field(i) => {
                let f = &self.fields[i as usize];
                d.kind = "data member".into();
                if let Some(t) = &f.ty {
                    row("Type", t.to_string());
                }
                row("Class", self.classes[f.owner as usize].name.to_string());
                if let Some(at) = self.at(f.loc) {
                    row("Declared at", at);
                }
            }
            Entity::Local(i) => {
                let l = &self.locals[i as usize];
                d.kind = "parameter or local variable".into();
                if let Some(t) = &l.ty {
                    row("Type", t.to_string());
                }
                row(
                    "Function",
                    self.functions[l.owner as usize].name.to_string(),
                );
                if let Some(at) = self.at(l.loc) {
                    row("Declared at", at);
                }
            }
            Entity::External(i) => {
                let x = &self.externals[i as usize];
                d.title = x.name.to_string();
                d.kind = "external".into();
                row("Linkage name", x.linkage.to_string());
                row(
                    "Defined",
                    "in no loaded object (a library, or an object not loaded yet)".into(),
                );
                lists.push(("Called by".into(), self.linked(e, false, EdgeClass::Calls)));
                lists.push((
                    "Used by".into(),
                    self.linked(e, false, EdgeClass::References),
                ));
            }
        }
        d.lists = lists.into_iter().filter(|(_, v)| !v.is_empty()).collect();
        d
    }
}

/// How definitions from different objects are matched.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum DefKey {
    /// The same name declared on the same line of the same file: one
    /// definition, even when compilers mangle it differently or emit
    /// several variants (GCC's complete and base constructors).
    Line(u32, u32, Arc<str>),
    /// Otherwise by linkage name, per file (two `static`s with one name
    /// in different files stay apart).
    Linkage(Arc<str>, Option<u32>),
}

fn def_key(display: &Arc<str>, linkage: &Arc<str>, loc: Option<Loc>) -> DefKey {
    match loc {
        Some(l) if l.line > 0 => DefKey::Line(l.file, l.line, display.clone()),
        _ => DefKey::Linkage(linkage.clone(), loc.map(|l| l.file)),
    }
}

fn key(e: Entity) -> (u8, u32) {
    match e {
        Entity::File(i) => (0, i),
        Entity::Class(i) => (1, i),
        Entity::Function(i) => (2, i),
        Entity::Global(i) => (3, i),
        Entity::Field(i) => (4, i),
        Entity::Local(i) => (5, i),
        Entity::External(i) => (6, i),
    }
}

fn typed(m: &MemberInfo) -> String {
    match &m.ty {
        Some(t) => format!("{}: {t}", m.name),
        None => m.name.to_string(),
    }
}

fn join_kind(kind: &str, extra: Option<String>) -> String {
    match extra {
        Some(x) => format!("{kind}  \u{b7}  {x}"),
        None => kind.to_string(),
    }
}

fn file_name(p: &Path) -> std::borrow::Cow<'_, str> {
    p.file_name()
        .map_or_else(|| p.to_string_lossy(), |n| n.to_string_lossy())
}

/// Collects one level's nodes and edges.
#[derive(Default)]
struct ModelBuilder {
    nodes: Vec<ModelNode>,
    edges: Vec<ModelEdge>,
    index: HashMap<Entity, u32>,
    hub: Option<u32>,
    hidden: usize,
}

impl ModelBuilder {
    fn add(&mut self, p: &Program, e: Entity, label: Option<String>) -> u32 {
        self.push(p, e, p.class_of(e), label)
    }

    fn push(&mut self, p: &Program, e: Entity, class: NodeClass, label: Option<String>) -> u32 {
        if let Some(&i) = self.index.get(&e) {
            return i;
        }
        let i = self.nodes.len() as u32;
        self.nodes.push(ModelNode {
            id: p.id_of(e).unwrap_or(NodeId(u64::from(i))),
            class,
            label: Arc::from(label.unwrap_or_else(|| p.name(e))),
            entity: Some(e),
        });
        self.index.insert(e, i);
        i
    }

    fn add_or_hide(&mut self, p: &Program, e: Entity, f: Filter, label: Option<String>) {
        if p.visible(e, f) {
            self.add(p, e, label);
        } else {
            self.hidden += 1;
        }
    }

    /// Something from another file, labelled with that file.
    fn add_outside(&mut self, p: &Program, e: Entity) {
        if !self.index.contains_key(&e) {
            self.push(p, e, NodeClass::External, Some(p.qualified_name(e)));
        }
    }

    fn contains(&self, e: Entity) -> bool {
        self.index.contains_key(&e)
    }

    /// Joins the hub to every node added so far.
    fn spokes(&mut self) {
        if let Some(hub) = self.hub {
            for i in 0..self.nodes.len() as u32 {
                self.edges.push(ModelEdge {
                    from: hub,
                    to: i,
                    class: EdgeClass::Structure,
                });
            }
        }
    }

    fn link(&mut self, from: Entity, to: Entity, class: EdgeClass) {
        if let (Some(&from), Some(&to)) = (self.index.get(&from), self.index.get(&to)) {
            self.edges.push(ModelEdge { from, to, class });
        }
    }

    fn finish(self) -> GraphModel {
        let mut m = GraphModel::new(self.nodes, self.edges, self.hub);
        m.hidden = self.hidden;
        m
    }
}

struct Builder {
    p: Program,
    root: Option<PathBuf>,
    file_keys: HashMap<String, u32>,
    classes_by_name: HashMap<Arc<str>, u32>,
    /// (path as debug info spells it, object) -> file.
    resolved: HashMap<(Arc<str>, PathBuf), u32>,
}

impl Builder {
    /// Adds `g`'s functions, globals, classes and their members, merging
    /// with what earlier objects defined. Returns each node's entity.
    fn definitions(&mut self, g: &CodeGraph) -> HashMap<NodeId, Entity> {
        let mut contains: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
        let mut methods: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
        let mut type_of: HashMap<NodeId, Arc<str>> = HashMap::new();
        for e in g.edges() {
            match e.kind {
                EdgeKind::Contains => contains.entry(e.from).or_default().push(e.to),
                EdgeKind::Method => methods.entry(e.from).or_default().push(e.to),
                EdgeKind::HasType => {
                    if let Some(t) = g.node(e.to) {
                        type_of.insert(e.from, t.display_name.clone());
                    }
                }
                _ => {}
            }
        }
        let members: HashSet<NodeId> = contains.values().flatten().copied().collect();
        let mut canon = HashMap::new();
        let mut functions: HashMap<DefKey, u32> = HashMap::new();
        for (i, f) in self.p.functions.iter().enumerate() {
            functions.insert(def_key(&f.name, &f.linkage, f.loc), i as u32);
        }
        let mut globals: HashMap<DefKey, u32> = HashMap::new();
        for (i, v) in self.p.globals.iter().enumerate() {
            globals.insert(def_key(&v.name, &v.linkage, v.loc), i as u32);
        }
        // Symbols without a source line (MSVC globals, functions without
        // line info) belong to the object's main source file.
        let primary = self.primary_file(g);
        // Not MSVC special names: `` `f'::`2'::x `` is a static local of a
        // function that may well be in a library header.
        let fallback = |n: &engine::Node| {
            primary
                .filter(|_| !n.library && !n.artificial && !n.display_name.contains('`'))
                .map(|file| Loc { file, line: 0 })
        };

        for n in g.nodes() {
            if members.contains(&n.id) {
                continue;
            }
            match n.kind {
                NodeKind::Function => {
                    let loc = self.loc(n.source.as_ref(), &g.path).or_else(|| fallback(n));
                    let k = def_key(&n.display_name, &n.linkage_name, loc);
                    let (i, new) = match functions.get(&k) {
                        Some(&i) => (i, false),
                        None => {
                            let i = self.p.functions.len() as u32;
                            functions.insert(k, i);
                            self.p.functions.push(FunctionInfo {
                                id: n.id,
                                name: n.display_name.clone(),
                                linkage: n.linkage_name.clone(),
                                loc,
                                size: n.size,
                                object: g.path.clone(),
                                class: None,
                                locals: Vec::new(),
                                library: n.library,
                                artificial: n.artificial,
                            });
                            (i, true)
                        }
                    };
                    canon.insert(n.id, Entity::Function(i));
                    for &l in contains.get(&n.id).into_iter().flatten() {
                        let Some(local) = g.node(l) else { continue };
                        let f = &self.p.functions[i as usize];
                        let existing = f
                            .locals
                            .iter()
                            .copied()
                            .find(|&j| self.p.locals[j as usize].name == local.display_name);
                        let j = match existing {
                            Some(j) if !new => j,
                            _ => {
                                let j = self.p.locals.len() as u32;
                                let loc = self.loc(local.source.as_ref(), &g.path);
                                self.p.locals.push(MemberInfo {
                                    id: local.id,
                                    name: local.display_name.clone(),
                                    owner: i,
                                    ty: type_of.get(&l).cloned(),
                                    loc,
                                });
                                self.p.functions[i as usize].locals.push(j);
                                j
                            }
                        };
                        canon.insert(l, Entity::Local(j));
                    }
                }
                NodeKind::Variable => {
                    let loc = self.loc(n.source.as_ref(), &g.path).or_else(|| fallback(n));
                    let k = def_key(&n.display_name, &n.linkage_name, loc);
                    let i = match globals.get(&k) {
                        Some(&i) => i,
                        None => {
                            let i = self.p.globals.len() as u32;
                            globals.insert(k, i);
                            self.p.globals.push(GlobalInfo {
                                id: n.id,
                                name: n.display_name.clone(),
                                linkage: n.linkage_name.clone(),
                                loc,
                                size: n.size,
                                object: g.path.clone(),
                                ty: type_of.get(&n.id).cloned(),
                                library: n.library,
                                artificial: n.artificial,
                            });
                            i
                        }
                    };
                    canon.insert(n.id, Entity::Global(i));
                }
                _ => {}
            }
        }

        // Classes: record types with a declaration or members. Merged by
        // name across objects.
        for n in g.nodes_of(NodeKind::Type) {
            let fields = contains.get(&n.id).map_or(&[][..], Vec::as_slice);
            let own_methods = methods.get(&n.id).map_or(&[][..], Vec::as_slice);
            if n.source.is_none() && fields.is_empty() && own_methods.is_empty() {
                continue;
            }
            let c = match self.classes_by_name.get(&n.display_name) {
                Some(&c) => c,
                None => {
                    let c = self.p.classes.len() as u32;
                    self.classes_by_name.insert(n.display_name.clone(), c);
                    let loc = self.loc(n.source.as_ref(), &g.path);
                    self.p.classes.push(ClassInfo {
                        id: hash_id("class", &n.display_name),
                        name: n.display_name.clone(),
                        loc,
                        size: n.size,
                        fields: Vec::new(),
                        methods: Vec::new(),
                        library: n.library,
                    });
                    c
                }
            };
            canon.insert(n.id, Entity::Class(c));
            for &fid in fields {
                let Some(field) = g.node(fid) else { continue };
                let existing = self.p.classes[c as usize]
                    .fields
                    .iter()
                    .copied()
                    .find(|&j| self.p.fields[j as usize].name == field.display_name);
                let j = match existing {
                    Some(j) => j,
                    None => {
                        let j = self.p.fields.len() as u32;
                        let loc = self.loc(field.source.as_ref(), &g.path);
                        self.p.fields.push(MemberInfo {
                            id: field.id,
                            name: field.display_name.clone(),
                            owner: c,
                            ty: type_of.get(&fid).cloned(),
                            loc,
                        });
                        self.p.classes[c as usize].fields.push(j);
                        j
                    }
                };
                canon.insert(fid, Entity::Field(j));
            }
            for m in own_methods {
                if let Some(&Entity::Function(f)) = canon.get(m) {
                    let class = &mut self.p.classes[c as usize];
                    if !class.methods.contains(&f) {
                        class.methods.push(f);
                    }
                    self.p.functions[f as usize].class.get_or_insert(c);
                }
            }
        }
        canon
    }

    /// The project file most of `g`'s functions come from.
    fn primary_file(&mut self, g: &CodeGraph) -> Option<u32> {
        let mut counts: HashMap<u32, usize> = HashMap::new();
        for n in g.nodes_of(NodeKind::Function) {
            if let (Some(src), false) = (&n.source, n.library || n.artificial) {
                *counts.entry(self.file_for(&src.file, &g.path)).or_default() += 1;
            }
        }
        counts
            .into_iter()
            .max_by_key(|&(f, n)| (n, std::cmp::Reverse(f)))
            .map(|(f, _)| f)
    }

    fn loc(&mut self, src: Option<&SourceLoc>, object: &Path) -> Option<Loc> {
        let src = src?;
        Some(Loc {
            file: self.file_for(&src.file, object),
            line: src.line,
        })
    }

    /// The file entry for a path as debug info spells it, resolved against
    /// the object's folder and the open folder.
    fn file_for(&mut self, raw: &Arc<str>, object: &Path) -> u32 {
        let dir = object.parent().unwrap_or(Path::new("")).to_path_buf();
        let cache = (raw.clone(), dir);
        let i = match self.resolved.get(&cache) {
            Some(&i) => i,
            None => {
                let found = resolve_source(raw, object, self.root.as_deref());
                let exists = found.is_some();
                let path = found.unwrap_or_else(|| normalize(Path::new(&**raw)));
                let i = self.file(path, exists, engine::is_library_path(raw));
                self.resolved.insert(cache, i);
                i
            }
        };
        self.p.files[i as usize]
            .objects
            .insert(object.to_path_buf());
        i
    }

    fn file(&mut self, path: PathBuf, exists: bool, library: bool) -> u32 {
        let key = path_key(&path);
        if let Some(&i) = self.file_keys.get(&key) {
            return i;
        }
        let i = self.p.files.len() as u32;
        self.p.files.push(FileInfo {
            id: hash_id("file", &key),
            name: Arc::from(&*file_name(&path)),
            path,
            exists,
            library,
            lines: None,
            bytes: None,
            includes: Vec::new(),
            included_by: Vec::new(),
            external_includes: Vec::new(),
            objects: BTreeSet::new(),
        });
        self.file_keys.insert(key, i);
        i
    }

    /// Reads every project source on disk: counts its lines and, for C and
    /// C++, follows its `#include` lines, adding headers that define no
    /// symbols of their own.
    fn scan_includes(&mut self) {
        let mut dirs: Vec<PathBuf> = Vec::new();
        let add_dir = |dirs: &mut Vec<PathBuf>, d: &Path| {
            if !dirs.iter().any(|x| x == d) {
                dirs.push(d.to_path_buf());
            }
        };
        for f in self.p.files.iter().filter(|f| f.exists && !f.library) {
            if let Some(d) = f.path.parent() {
                add_dir(&mut dirs, d);
            }
        }
        if let Some(root) = self.root.clone() {
            add_dir(&mut dirs, &root);
            add_dir(&mut dirs, &root.join("include"));
        }
        let mut queue: VecDeque<u32> = (0..self.p.files.len() as u32).collect();
        while let Some(i) = queue.pop_front() {
            let f = &self.p.files[i as usize];
            if !f.exists || f.library {
                continue;
            }
            let path = f.path.clone();
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            self.p.files[i as usize].bytes = Some(meta.len());
            if meta.len() > MAX_SCAN_BYTES {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let text = String::from_utf8_lossy(&bytes);
            self.p.files[i as usize].lines = Some(text.lines().count());
            if !is_c_family(&path) {
                continue;
            }
            let own_dir = path.parent().map(Path::to_path_buf);
            for (target, angle) in parse_includes(&text) {
                // Quoted includes look next to the includer first.
                let mut search = Vec::new();
                if !angle {
                    search.extend(own_dir.clone());
                }
                search.extend(dirs.iter().cloned());
                let found = search
                    .iter()
                    .map(|d| normalize(&d.join(&target)))
                    .find(|c| c.is_file());
                let Some(found) = found else {
                    self.p.files[i as usize].external_includes.push(target);
                    continue;
                };
                let library = engine::is_library_path(&found.to_string_lossy());
                let known = self.file_keys.contains_key(&path_key(&found));
                if !known && self.p.files.len() >= MAX_FILES {
                    continue;
                }
                if let Some(d) = found.parent() {
                    add_dir(&mut dirs, d);
                }
                let j = self.file(found, true, library);
                if !known {
                    queue.push_back(j);
                }
                if j != i && !self.p.files[i as usize].includes.contains(&j) {
                    self.p.files[i as usize].includes.push(j);
                    self.p.files[j as usize].included_by.push(i);
                }
            }
        }
    }
}

/// `#include "a.h"` and `#include <a.h>` targets, with whether they used
/// angle brackets. Not a preprocessor: conditionals are ignored, so both
/// branches of an `#if` count.
pub fn parse_includes(text: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.trim_start().strip_prefix('#') else {
            continue;
        };
        let Some(rest) = rest.trim_start().strip_prefix("include") else {
            continue;
        };
        let rest = rest.trim_start();
        let (close, angle) = match rest.chars().next() {
            Some('"') => ('"', false),
            Some('<') => ('>', true),
            _ => continue,
        };
        if let Some(end) = rest[1..].find(close) {
            let target = rest[1..1 + end].trim();
            if !target.is_empty() {
                out.push((target.to_string(), angle));
            }
        }
    }
    out
}

fn is_c_family(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| C_FAMILY.contains(&e.to_ascii_lowercase().as_str()))
}

/// Lexically removes `.` and `..` components.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// Identity of a path: separators unified, and case-folded on Windows.
fn path_key(p: &Path) -> String {
    let s = normalize(p).to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s
    }
}

fn hash_id(kind: &str, name: &str) -> NodeId {
    // FNV-1a: stable across runs and platforms.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in kind.bytes().chain([0]).chain(name.bytes()) {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    NodeId(h)
}

/// Finds a source file: absolute paths as they are; relative ones (DWARF
/// with a remapped prefix) against the object's folder and its parents,
/// then the open folder.
pub fn resolve_source(file: &str, object: &Path, root: Option<&Path>) -> Option<PathBuf> {
    // Rebuild from components so mixed separators come out uniform.
    let p = normalize(Path::new(file));
    if p.is_absolute() {
        return p.is_file().then_some(p);
    }
    object
        .ancestors()
        .skip(1)
        .take(5)
        .chain(root)
        .map(|dir| normalize(&dir.join(&p)))
        .find(|c| c.is_file())
}

/// `Shape::area` from `Shape::area() const` or from MSVC's
/// `public: virtual double __cdecl Shape::area(void) const`: the parameter
/// list and anything before the name are dropped, respecting templates.
pub fn short_name(display: &str) -> String {
    let mut depth = 0i32;
    let mut cut = display.len();
    for (i, ch) in display.char_indices() {
        match ch {
            '<' => depth += 1,
            '>' => depth -= 1,
            '(' if depth <= 0 && !display[..i].ends_with("operator") => {
                cut = i;
                break;
            }
            _ => {}
        }
    }
    let head = display[..cut].trim_end();
    // The last space outside template brackets ends the return type and
    // qualifiers.
    let mut depth = 0i32;
    let mut start = 0;
    for (i, ch) in head.char_indices() {
        match ch {
            '<' => depth += 1,
            '>' => depth -= 1,
            ' ' if depth <= 0 => start = i + 1,
            _ => {}
        }
    }
    // `operator delete`, `Foo::operator new[]`: the keyword belongs to it.
    if let Some(op) = head[..start].trim_end().strip_suffix("operator") {
        let at = op.len();
        start = head[..at].rfind(' ').map_or(0, |i| i + 1);
    }
    let name = &head[start..];
    if name.is_empty() {
        display.to_string()
    } else {
        name.to_string()
    }
}

/// A method's label inside its class: `area` for `Shape::area`.
fn method_label(name: &str, class: &str) -> String {
    name.strip_prefix(class)
        .and_then(|rest| rest.strip_prefix("::"))
        .unwrap_or(name)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Arc<CodeGraph> {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../engine/tests/fixtures")
            .join(name);
        Arc::new(engine::load(&p).expect("fixture loads"))
    }

    fn labels(m: &GraphModel) -> Vec<&str> {
        let mut v: Vec<&str> = m.nodes().iter().map(|n| &*n.label).collect();
        v.sort_unstable();
        v
    }

    fn edge(m: &GraphModel, from: &str, to: &str, class: EdgeClass) -> bool {
        m.edges().iter().any(|e| {
            e.class == class
                && &*m.nodes()[e.from as usize].label == from
                && &*m.nodes()[e.to as usize].label == to
        })
    }

    fn file_level(p: &Program, name: &str) -> Level {
        let f = p
            .files
            .iter()
            .find(|f| &*f.name == name)
            .unwrap_or_else(|| panic!("{name} not among the files"));
        Level::File(f.id)
    }

    #[test]
    fn short_names_drop_parameters_and_qualifiers() {
        assert_eq!(short_name("Square::area() const"), "Square::area");
        assert_eq!(
            short_name("public: virtual double __cdecl Shape::area(void) const"),
            "Shape::area"
        );
        assert_eq!(short_name("measure(Shape const&)"), "measure");
        assert_eq!(
            short_name("std::map<int, std::pair<a, b> >::find(int const&)"),
            "std::map<int, std::pair<a, b> >::find"
        );
        assert_eq!(short_name("Foo::operator()(int)"), "Foo::operator()");
        assert_eq!(
            short_name("operator delete(void*, unsigned long long)"),
            "operator delete"
        );
        assert_eq!(short_name("operator delete"), "operator delete");
        assert_eq!(short_name("main"), "main");
        assert_eq!(method_label("Shape::area", "Shape"), "area");
    }

    #[test]
    fn includes_are_parsed_from_directives_only() {
        let text = "#include \"a.h\"\n  #  include <vector>\n// #include \"no.h\"\n#define X\n#include MACRO\n";
        assert_eq!(
            parse_includes(text),
            [("a.h".to_string(), false), ("vector".to_string(), true)]
        );
    }

    #[test]
    fn files_level_shows_sources_with_include_edges() {
        let dir = std::env::temp_dir().join(format!("lattice-includes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("inc")).expect("temp dir");
        std::fs::write(
            dir.join("main.c"),
            "#include \"inc/a.h\"\n#include <stdio.h>\nint main(){}\n",
        )
        .expect("write");
        std::fs::write(dir.join("inc/a.h"), "#include \"b.h\"\n").expect("write");
        std::fs::write(dir.join("inc/b.h"), "#include \"a.h\"\n").expect("write");

        let mut b = Builder {
            p: Program::default(),
            root: Some(dir.clone()),
            file_keys: HashMap::new(),
            classes_by_name: HashMap::new(),
            resolved: HashMap::new(),
        };
        b.file(dir.join("main.c"), true, false);
        b.scan_includes();
        let p = b.p;
        let m = p.model(Level::Files, Filter::default());
        assert_eq!(labels(&m), ["a.h", "b.h", "main.c"]);
        assert!(edge(&m, "main.c", "a.h", EdgeClass::Includes));
        assert!(edge(&m, "a.h", "b.h", EdgeClass::Includes));
        assert!(edge(&m, "b.h", "a.h", EdgeClass::Includes));
        let main = &p.files[0];
        assert_eq!(main.lines, Some(3));
        assert_eq!(main.external_includes, ["stdio.h"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_level_shows_functions_globals_and_outside_calls() {
        let p = Program::build(&[fixture("mingw_dwarf.o"), fixture("mingw_cpp.o")], None);
        let files = p.model(Level::Files, Filter::default());
        let names = labels(&files);
        assert!(names.contains(&"graph.c"), "{names:?}");
        assert!(names.contains(&"shapes.cpp"), "{names:?}");

        // Abstracted: only what the user wrote.
        let m = p.model(file_level(&p, "graph.c"), GraphMode::Abstracted.filter());
        assert!(!labels(&m).contains(&"external_fn"), "{:?}", labels(&m));
        assert!(labels(&m).contains(&"caller"));

        let m = p.model(file_level(&p, "graph.c"), GraphMode::Verbose.filter());
        let hub = m.hub.expect("file hub");
        assert_eq!(&*m.nodes()[hub as usize].label, "graph.c");
        for want in ["caller", "callee", "helper", "counter", "read_counter"] {
            assert!(labels(&m).contains(&want), "{want} in {:?}", labels(&m));
        }
        assert!(edge(&m, "caller", "callee", EdgeClass::Calls));
        assert!(edge(&m, "graph.c", "caller", EdgeClass::Structure));
        // Undefined: an outside node, without a spoke.
        let ext = m
            .nodes()
            .iter()
            .position(|n| &*n.label == "external_fn")
            .expect("external_fn");
        assert_eq!(m.nodes()[ext].class, NodeClass::External);
        assert!(edge(&m, "caller", "external_fn", EdgeClass::Calls));
        assert!(!edge(&m, "graph.c", "external_fn", EdgeClass::Structure));
    }

    #[test]
    fn classes_drill_into_members_and_functions_into_locals() {
        for object in ["mingw_cpp.o", "msvc_cpp.obj"] {
            let p = Program::build(&[fixture(object)], None);
            let m = p.model(file_level(&p, "shapes.cpp"), Filter::default());
            let names = labels(&m);
            for want in ["Shape", "Square", "main", "measure", "total", "unit"] {
                assert!(names.contains(&want), "{object}: {want} in {names:?}");
            }
            // Globals without a line (CodeView) still live in their file.
            let total = m.nodes().iter().find(|n| &*n.label == "total");
            assert_eq!(
                total.map(|n| n.class),
                Some(NodeClass::Variable),
                "{object}"
            );
            // Methods are inside their class, not beside it.
            assert!(
                !names.iter().any(|n| n.contains("area")),
                "{object}: {names:?}"
            );
            // main constructs a Square: the call lands on the class.
            assert!(edge(&m, "main", "Square", EdgeClass::Calls), "{object}");

            let square = m
                .nodes()
                .iter()
                .find(|n| &*n.label == "Square")
                .and_then(|n| n.entity)
                .expect("Square");
            let level = p.level_of(square).expect("class level");
            let c = p.model(level, Filter::default());
            let names = labels(&c);
            assert!(names.contains(&"s"), "{object}: {names:?}");
            // GCC's complete and base constructors are one node.
            let ctors = c
                .nodes()
                .iter()
                .filter(|n| &*n.label == "Square" && n.class == NodeClass::Function)
                .count();
            assert_eq!(ctors, 1, "{object}: {names:?}");
            assert!(
                names.iter().any(|n| n.starts_with("area")),
                "{object}: {names:?}"
            );
            let crumbs: Vec<String> = p.breadcrumb(level).into_iter().map(|c| c.0).collect();
            assert_eq!(crumbs, ["Files", "shapes.cpp", "Square"], "{object}");

            let main = p
                .functions
                .iter()
                .position(|f| &*f.linkage == "main")
                .expect("main");
            let level = p.level_of(Entity::Function(main as u32)).expect("level");
            let f = p.model(level, Filter::default());
            for want in ["count", "sq", "sum"] {
                assert!(labels(&f).contains(&want), "{object}: {want}");
            }
            assert!(edge(&f, "main", "sq", EdgeClass::Structure));
        }
    }

    #[test]
    fn details_describe_each_kind() {
        let p = Program::build(&[fixture("mingw_cpp.o")], None);
        let class = p
            .classes
            .iter()
            .position(|c| &*c.name == "Square")
            .expect("Square");
        let d = p.details(Entity::Class(class as u32));
        assert!(d.kind.starts_with("class"), "{d:?}");
        assert!(d.kind.contains("16 bytes"), "{d:?}");
        assert!(d
            .lists
            .iter()
            .any(|(h, v)| h == "Data members" && v.contains(&"s: double".to_string())));
        assert!(d.source.is_some(), "fixture source is on disk");

        let measure = p
            .functions
            .iter()
            .position(|f| f.name.starts_with("measure"))
            .expect("measure");
        let d = p.details(Entity::Function(measure as u32));
        let list = |h: &str| d.lists.iter().find(|(x, _)| x == h).map(|(_, v)| v.clone());
        assert!(list("Called by").is_some_and(|v| v.iter().any(|s| s.starts_with("main"))));
        assert!(list("Parameters and locals").is_some_and(|v| v.contains(&"a: double".into())));
        assert!(d.rows.iter().any(|(k, _)| k == "Defined at"));
    }

    #[test]
    fn levels_survive_a_rebuild_and_parents_lead_home() {
        let a = Program::build(&[fixture("mingw_dwarf.o")], None);
        let level = file_level(&a, "graph.c");
        let b = Program::build(
            &[fixture("mingw_dwarf.o"), fixture("mingw_external.o")],
            None,
        );
        assert!(b.has_level(level));
        assert_eq!(b.parent(level), Some(Level::Files));
        assert_eq!(b.parent(Level::Files), None);
        assert!(!b.has_level(Level::Class(NodeId(1))));
    }

    #[test]
    fn library_and_generated_are_filtered() {
        let p = Program::build(&[fixture("msvc_cpp.obj")], None);
        let off = p.model(Level::Files, Filter::default());
        assert!(!labels(&off).contains(&"stdio.h"));
        assert!(off.hidden > 0);
        let on = p.model(Level::Files, GraphMode::Verbose.filter());
        assert!(labels(&on).contains(&"stdio.h"), "{:?}", labels(&on));
    }

    #[test]
    fn relative_sources_resolve_against_object_folder() {
        let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../engine/tests/fixtures");
        let object = fixtures.join("mingw_dwarf.o");
        let found = resolve_source("./src/graph.c", &object, None).expect("found");
        assert!(found.ends_with("src/graph.c"));
        assert_eq!(resolve_source("./src/missing.c", &object, None), None);
        let abs = fixtures.join("src").join("graph.c");
        let abs = abs.to_string_lossy();
        assert!(resolve_source(&abs, Path::new("x.o"), None).is_some());
    }
}
