//! Power-loss file-system model (test only; design section 8 R3).
//!
//! `CrashingOps` runs every [`FileOps`] call against a real directory and, in
//! parallel, tracks what a POSIX file system has *durably* promised:
//!
//! - file content is durable only up to the last `fsync` of that inode
//!   (`write_new_synced`, `copy_new_synced`, `fsync_file`);
//! - a directory's entries are durable only as of the last `fsync_dir` of
//!   that directory. A `rename`, a new file or a new sub-directory is
//!   therefore lost unless the directory holding the entry is fsynced; a
//!   rename across two directories needs both. The old name stays durable
//!   until its own directory is fsynced, so a file may legally show up under
//!   both names after a crash;
//! - a new directory's own entries (not its name) are durable only after that
//!   directory itself was fsynced;
//! - everything that exists when the model starts counts as durable.
//!
//! After every call (and on `boundary()`) it records the on-disk image a crash
//! would leave, under a [`Policy`]: content that was written but never fsynced
//! is dropped (last synced bytes), fully persisted, or torn in half; directory
//! entries are the fsynced ones (POSIX minimum) or all current ones. A seeded
//! policy decides both independently per directory / per inode, so outcomes
//! like "one of two new files survives" are sampled too. Hard links survive
//! the reboot as hard links (alias classes in [`Node::File`]).
//! Writes done outside `FileOps` (SQLite, `mkdir`, `set_permissions`) are
//! volatile until a covering fsync, which is conservative.
//!
//! Inodes are pinned by a hard link outside the tree so inode numbers cannot
//! be reused while the model still refers to them.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use super::atomic::FileOps;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Node {
    Dir,
    /// Bytes and an alias class: equal classes are hard links of one inode.
    File(Vec<u8>, u32),
}
/// Paths relative to the model root; the root itself is implicit.
pub type Image = BTreeMap<PathBuf, Node>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Unsynced {
    Drop,
    Keep,
    Half,
}

/// One way a crash can resolve what was never made durable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    pub content: Unsynced,
    /// `false`: only fsynced directory entries survive (POSIX minimum).
    /// `true`: every current entry survives (metadata reaches disk in
    /// order), so only missing *content* syncs can bite.
    pub entries_current: bool,
    /// Per-directory / per-inode independent outcomes (overrides the two
    /// fields above) drawn from this seed.
    pub seed: Option<u64>,
}

fn coin(seed: u64, key: u64, salt: u64) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (seed, key, salt).hash(&mut h);
    h.finish()
}

pub type Pred = Rc<dyn Fn(&Path) -> bool>;

/// Deliberate omissions, to prove the model catches a missing fsync.
#[derive(Clone, Default)]
pub struct Mutation {
    /// File fsyncs (any path matching) that silently do nothing.
    pub skip_file_sync: Option<Pred>,
    /// Directory fsyncs (any directory matching) that silently do nothing.
    pub skip_dir_sync: Option<Pred>,
}

#[derive(Default)]
struct State {
    root_ino: u64,
    synced: HashMap<u64, Vec<u8>>,
    dirs: HashMap<u64, BTreeMap<String, (u64, bool)>>,
    dir_paths: HashMap<u64, PathBuf>,
    pinned: HashMap<u64, PathBuf>,
    calls: Vec<String>,
    images: Vec<(String, Policy, Image)>,
}

pub struct CrashingOps {
    root: PathBuf,
    pin: tempfile::TempDir,
    policies: Vec<Policy>,
    mutation: Mutation,
    st: RefCell<State>,
}

impl CrashingOps {
    /// Everything under `root` right now is durable.
    pub fn new(root: &Path, policies: &[Policy], mutation: Mutation) -> Self {
        let this = Self {
            root: root.to_path_buf(),
            pin: tempfile::tempdir().expect("pin dir"),
            policies: policies.to_vec(),
            mutation,
            st: RefCell::default(),
        };
        let ino = fs::metadata(root).expect("root").ino();
        this.st.borrow_mut().root_ino = ino;
        this.adopt(root);
        this
    }

    fn adopt(&self, dir: &Path) {
        let ino = fs::metadata(dir).expect("dir").ino();
        let entries = self.scan(dir);
        for (name, (_, is_dir)) in &entries {
            let p = dir.join(name);
            if *is_dir {
                self.adopt(&p);
            } else {
                self.set_synced(&p);
            }
        }
        let mut st = self.st.borrow_mut();
        st.dirs.insert(ino, entries);
        st.dir_paths.insert(ino, dir.to_path_buf());
    }

    /// Pin every file and remember every directory path that exists now.
    fn observe(&self, dir: &Path) {
        let Ok(md) = fs::metadata(dir) else { return };
        self.st
            .borrow_mut()
            .dir_paths
            .insert(md.ino(), dir.to_path_buf());
        for (name, (_, is_dir)) in self.scan(dir) {
            if is_dir {
                self.observe(&dir.join(name));
            }
        }
    }

    /// Current listing of `dir`; pins every file inode it sees.
    fn scan(&self, dir: &Path) -> BTreeMap<String, (u64, bool)> {
        let mut out = BTreeMap::new();
        for ent in fs::read_dir(dir).expect("read_dir") {
            let Ok(ent) = ent else { continue };
            // SQLite worker threads may unlink a journal while we list.
            let Ok(md) = fs::symlink_metadata(ent.path()) else {
                continue;
            };
            let is_dir = md.is_dir();
            if !is_dir {
                self.pin_file(&ent.path(), md.ino());
            }
            out.insert(
                ent.file_name().to_string_lossy().into_owned(),
                (md.ino(), is_dir),
            );
        }
        out
    }

    fn pin_file(&self, p: &Path, ino: u64) {
        let mut st = self.st.borrow_mut();
        if let std::collections::hash_map::Entry::Vacant(e) = st.pinned.entry(ino) {
            let link = self.pin.path().join(ino.to_string());
            if fs::hard_link(p, &link).is_ok() {
                e.insert(link);
            }
        }
    }

    fn set_synced(&self, p: &Path) {
        if let Ok(md) = fs::metadata(p) {
            self.pin_file(p, md.ino());
            if let Ok(b) = fs::read(p) {
                self.st.borrow_mut().synced.insert(md.ino(), b);
            }
        }
    }

    fn file_sync(&self, p: &Path) {
        if self.mutation.skip_file_sync.as_ref().is_some_and(|f| f(p)) {
            if let Ok(md) = fs::metadata(p) {
                self.pin_file(p, md.ino());
            }
        } else {
            self.set_synced(p);
        }
    }

    fn content(
        &self,
        st: &State,
        cache: &mut HashMap<u64, Vec<u8>>,
        pol: Policy,
        ino: u64,
    ) -> Vec<u8> {
        let cur = cache.entry(ino).or_insert_with(|| {
            st.pinned
                .get(&ino)
                .and_then(|p| fs::read(p).ok())
                .unwrap_or_default()
        });
        let synced = st.synced.get(&ino).map(Vec::as_slice).unwrap_or_default();
        let how = match pol.seed {
            Some(sd) => {
                [Unsynced::Drop, Unsynced::Keep, Unsynced::Half][(coin(sd, ino, 1) % 3) as usize]
            }
            None => pol.content,
        };
        match how {
            Unsynced::Keep => cur.clone(),
            Unsynced::Drop => synced.to_vec(),
            // torn write: durable prefix kept, half of what was appended or
            // rewritten on top of it
            Unsynced::Half if cur.as_slice() == synced => cur.clone(),
            Unsynced::Half if cur.starts_with(synced) => {
                cur[..synced.len() + (cur.len() - synced.len()) / 2].to_vec()
            }
            Unsynced::Half => cur[..cur.len() / 2].to_vec(),
        }
    }

    /// Entries of directory `ino` as the disk would hold them.
    fn entries(
        &self,
        st: &State,
        plan: &HashMap<u64, BTreeMap<String, (u64, bool)>>,
        pol: Policy,
        ino: u64,
    ) -> BTreeMap<String, (u64, bool)> {
        if pol.seed.is_some() {
            return plan
                .get(&ino)
                .cloned()
                .unwrap_or_else(|| st.dirs.get(&ino).cloned().unwrap_or_default());
        }
        let live = st
            .dir_paths
            .get(&ino)
            .filter(|p| pol.entries_current && fs::metadata(p).is_ok_and(|m| m.ino() == ino));
        match live {
            Some(path) => list_real(path),
            None => st.dirs.get(&ino).cloned().unwrap_or_default(),
        }
    }

    /// Seeded namespace outcome. Every difference between the durable and
    /// the current listings is an operation that may or may not have reached
    /// disk, independently of the others, EXCEPT that a rename is atomic: the
    /// removal from the old directory and the addition to the new one (same
    /// inode) stand or fall together, across directories.
    fn plan(&self, st: &State, seed: u64) -> HashMap<u64, BTreeMap<String, (u64, bool)>> {
        type Slot = Option<(u64, bool)>;
        let mut units: Vec<(u64, String, Slot, Slot)> = vec![];
        let mut plan = HashMap::new();
        let mut cur_loc: HashMap<u64, Vec<u64>> = HashMap::new();
        let mut parent_of: HashMap<u64, u64> = HashMap::new();
        for (dino, path) in &st.dir_paths {
            if !fs::metadata(path).is_ok_and(|m| m.ino() == *dino) {
                continue;
            }
            let (dur, cur) = (
                st.dirs.get(dino).cloned().unwrap_or_default(),
                list_real(path),
            );
            for name in dur
                .keys()
                .chain(cur.keys())
                .collect::<std::collections::BTreeSet<_>>()
            {
                let (o, n) = (dur.get(name).copied(), cur.get(name).copied());
                if o != n {
                    units.push((*dino, name.clone(), o, n));
                }
            }
            for (ino, is_dir) in cur.values().copied() {
                if is_dir {
                    parent_of.insert(ino, *dino);
                } else {
                    cur_loc.entry(ino).or_default().push(*dino);
                }
            }
            plan.insert(*dino, dur);
        }
        // union-find: units that move the same inode are one operation
        let mut parent: Vec<usize> = (0..units.len()).collect();
        fn find(p: &mut [usize], mut i: usize) -> usize {
            while p[i] != i {
                p[i] = p[p[i]];
                i = p[i];
            }
            i
        }
        let mut owner: HashMap<u64, usize> = HashMap::new();
        let mut created: HashMap<u64, usize> = HashMap::new();
        for (i, u) in units.iter().enumerate() {
            if let (None, Some((ino, true))) = (u.2, u.3) {
                created.insert(ino, i);
            }
            for ino in [u.2, u.3].into_iter().flatten().map(|x| x.0) {
                if let Some(&j) = owner.get(&ino) {
                    let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                    parent[a] = b;
                } else {
                    owner.insert(ino, i);
                }
            }
        }
        // an entry inside a directory that is itself new needs the directory,
        // and a rename out of a directory needs the (new) directory it went to
        for (i, u) in units.iter().enumerate() {
            let mut need = vec![u.0];
            if let (Some((ino, _)), None) = (u.2, u.3) {
                need.extend(cur_loc.get(&ino).into_iter().flatten().copied());
            }
            for mut d in need {
                // every ancestor must exist on disk too
                loop {
                    if let Some(&j) = created.get(&d) {
                        let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                        parent[a] = b;
                    }
                    match parent_of.get(&d) {
                        Some(&up) => d = up,
                        None => break,
                    }
                }
            }
        }
        for (i, (dino, name, _, new)) in units.into_iter().enumerate() {
            let key = find(&mut parent, i) as u64;
            if coin(seed, key, 2).is_multiple_of(2) {
                let dir = plan.entry(dino).or_default();
                match new {
                    Some(v) => dir.insert(name, v),
                    None => dir.remove(&name),
                };
            }
        }
        plan
    }

    #[allow(clippy::too_many_arguments)]
    fn walk(
        &self,
        st: &State,
        cache: &mut HashMap<u64, Vec<u8>>,
        plan: &HashMap<u64, BTreeMap<String, (u64, bool)>>,
        pol: Policy,
        dir: u64,
        rel: &Path,
        out: &mut BTreeMap<PathBuf, Option<(Vec<u8>, u64)>>,
    ) {
        for (name, (ino, is_dir)) in self.entries(st, plan, pol, dir) {
            let p = rel.join(&name);
            if is_dir {
                out.insert(p.clone(), None);
                self.walk(st, cache, plan, pol, ino, &p, out);
            } else if st.pinned.contains_key(&ino) {
                out.insert(p, Some((self.content(st, cache, pol, ino), ino)));
            }
        }
    }

    /// What a crash right now would leave under `pol`.
    pub fn image(&self, pol: Policy) -> Image {
        self.observe(&self.root);
        self.image_cached(pol, &mut HashMap::new())
    }

    fn image_cached(&self, pol: Policy, cache: &mut HashMap<u64, Vec<u8>>) -> Image {
        let st = self.st.borrow();
        let mut raw = BTreeMap::new();
        let plan = pol.seed.map(|sd| self.plan(&st, sd)).unwrap_or_default();
        self.walk(&st, cache, &plan, pol, st.root_ino, Path::new(""), &mut raw);
        // alias classes numbered by first appearance (stable across runs)
        let mut class: HashMap<u64, u32> = HashMap::new();
        raw.into_iter()
            .map(|(p, f)| match f {
                None => (p, Node::Dir),
                Some((bytes, ino)) => {
                    let n = class.len() as u32;
                    let c = *class.entry(ino).or_insert(n);
                    (p, Node::File(bytes, c))
                }
            })
            .collect()
    }

    /// Record a crash point (also called after every `FileOps` call).
    pub fn boundary(&self, label: &str) {
        if self.policies.is_empty() {
            return;
        }
        self.observe(&self.root);
        let mut cache = HashMap::new();
        for &pol in &self.policies {
            let img = self.image_cached(pol, &mut cache);
            let mut st = self.st.borrow_mut();
            let dup = st
                .images
                .iter()
                .rev()
                .find(|(_, p, _)| *p == pol)
                .is_some_and(|(_, _, last)| *last == img);
            if !dup {
                st.images.push((label.to_owned(), pol, img));
            }
        }
    }

    fn record(&self, op: &str, p: &Path) {
        let n = {
            let mut st = self.st.borrow_mut();
            st.calls.push(format!(
                "{op}:{}",
                p.strip_prefix(&self.root).unwrap_or(p).display()
            ));
            st.calls.len()
        };
        self.boundary(&format!(
            "#{n} {op} {}",
            p.strip_prefix(&self.root).unwrap_or(p).display()
        ));
    }

    pub fn image_count(&self) -> usize {
        self.st.borrow().images.len()
    }
    pub fn calls(&self) -> Vec<String> {
        self.st.borrow().calls.clone()
    }
    pub fn take_images(&self) -> Vec<(String, Policy, Image)> {
        std::mem::take(&mut self.st.borrow_mut().images)
    }
}

/// Real file effects, but never a real `fsync` (macOS `F_FULLFSYNC` is
/// milliseconds each): durability is the model's job, not the kernel's.
fn create_new(path: &Path, src: &mut dyn io::Read) -> io::Result<()> {
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    io::copy(src, &mut f).map(drop)
}

impl FileOps for CrashingOps {
    fn write_new_synced(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        create_new(path, &mut &*data)?;
        // a mutant "writes but forgets the fsync": file_sync honours that
        self.file_sync(path);
        self.record("write", path);
        Ok(())
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(from, to)?;
        self.record("rename", to);
        Ok(())
    }
    fn fsync_file(&self, path: &Path) -> io::Result<()> {
        fs::File::open(path)?;
        self.file_sync(path);
        self.record("fsync_file", path);
        Ok(())
    }
    fn fsync_dir(&self, dir: &Path) -> io::Result<()> {
        fs::File::open(dir)?;
        if !self.mutation.skip_dir_sync.as_ref().is_some_and(|f| f(dir)) {
            let ino = fs::metadata(dir)?.ino();
            let entries = self.scan(dir);
            self.st.borrow_mut().dirs.insert(ino, entries);
        }
        self.record("fsync_dir", dir);
        Ok(())
    }
    fn remove_file(&self, path: &Path) -> io::Result<()> {
        fs::remove_file(path)?;
        self.record("remove", path);
        Ok(())
    }
    fn copy_new_synced(&self, from: &Path, to: &Path) -> io::Result<()> {
        create_new(to, &mut fs::File::open(from)?)?;
        self.file_sync(to);
        self.record("copy", to);
        Ok(())
    }
}

fn list_real(path: &Path) -> BTreeMap<String, (u64, bool)> {
    let mut out = BTreeMap::new();
    for ent in fs::read_dir(path).into_iter().flatten().flatten() {
        if let Ok(md) = fs::symlink_metadata(ent.path()) {
            out.insert(
                ent.file_name().to_string_lossy().into_owned(),
                (md.ino(), md.is_dir()),
            );
        }
    }
    out
}

/// Materialise `img` into the (empty) directory `dir`: a rebooted disk.
pub fn write_image(img: &Image, dir: &Path) {
    let mut first: HashMap<u32, PathBuf> = HashMap::new();
    for (rel, node) in img {
        let p = dir.join(rel);
        match node {
            Node::Dir => fs::DirBuilder::new().mode(0o700).create(&p).expect("mkdir"),
            Node::File(b, class) => {
                if let Some(orig) = first.get(class) {
                    fs::hard_link(orig, &p).expect("alias");
                    continue;
                }
                let mut f = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&p)
                    .expect("create");
                io::Write::write_all(&mut f, b).expect("write");
                first.insert(*class, p);
            }
        }
    }
}
