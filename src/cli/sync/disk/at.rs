//! [`super::scan`]'s walk on linux, with each folder opened in the one above it and each file
//! looked at in its open folder, never by its whole path, so no look walks the path again and no
//! open costs a second call to close it on exec. a large walk runs on the run's threads. it finds
//! what `walk_by_paths`, the walk on other systems, finds, in the same order.

use std::collections::HashMap;
use std::ffi::{CStr, CString, OsStr};
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::Arc;

use rustix::fs::{AtFlags, FileType, Mode, OFlags, RawDir, Stat};

use super::{Entry, Scanned, push_alone};
use crate::place;
use crate::project::state::Look;
use crate::threads::Threads;

/// the walk starts helpers once it has this many folders waiting to be listed...
const HELPED_FROM_FOLDERS: usize = 32;
/// ...or has listed this many files: below both, it walks alone, as a small project needs no
/// thread.
const HELPED_FROM_FILES: usize = 1_000;
/// files listed for each helper the walk keeps, four times git's preload's 500 looks a thread,
/// since each helper's heap holds memory of its own; folders waiting count for 32 files each.
const FILES_A_HELPER: usize = 2_000;
/// the most files one job looks at, so a large folder's looks spread over the helpers.
const RUN: usize = 256;
/// the most helpers a walk keeps: past four, the walk's own thread, which makes every path and
/// records every look, is what it waits on, and each helper's heap adds to the run's peak.
const HELPERS: usize = 4;
/// bytes of listing one call to the system reads at most, into a buffer each lister reuses.
const LISTING_BUFFER: usize = 32 << 10;

/// what a helper lists into and looks into: buffers this thread makes and takes back, so a
/// helper allocates nothing of its own but to grow one, and each thread's heap the allocator
/// keeps stays this thread's.
#[derive(Default)]
struct Buffers {
    /// the names of a folder's entries or of files to look at, one after another, each ended by
    /// a nul.
    names: Vec<u8>,
    /// for each entry of a listing, where its name starts, its kind, and for the first [`RUN`]
    /// files what a look found, `None` past them, left to [`Job::Look`].
    entries: Vec<(usize, FileType, Option<Looked>)>,
    /// for each file to look at, where its name starts.
    starts: Vec<usize>,
    looks: Vec<Looked>,
}

/// what a look at a file finds: none when it is gone, else its look, whether any may write it,
/// and whether it is read-only with no other name.
type Looked = Option<(Look, bool, bool)>;

/// a folder to list: one the walk starts from, by its path on disk, which may be a link the
/// survey follows, or one a listing found, by its name in its open parent, never followed.
enum Place {
    Start(PathBuf),
    Below(Arc<OwnedFd>, CString),
}

/// work the walk hands to a helper, or does itself.
enum Job {
    /// list the folder of the stack's slot into the buffers, looking at its first files.
    List(usize, Place, Buffers),
    /// look at the files the buffers name in one open folder, for the batch numbered so.
    Look(usize, Arc<OwnedFd>, Buffers),
}

/// what a job gives back: the folder it opened, if it would open, with the buffers filled.
enum Done {
    Listed(usize, Option<OwnedFd>, Buffers),
    Looked(usize, Buffers),
}

fn run(job: Job, listing: &mut [MaybeUninit<u8>]) -> Done {
    match job {
        Job::List(slot, place, mut buffers) => {
            let dir = list_into(place, listing, &mut buffers);
            Done::Listed(slot, dir, buffers)
        }
        Job::Look(batch, dir, mut buffers) => {
            let Buffers {
                names,
                starts,
                looks,
                ..
            } = &mut buffers;
            looks.extend(
                starts
                    .iter()
                    .map(|start| looked_at(&dir, named(names, *start))),
            );
            Done::Looked(batch, buffers)
        }
    }
}

/// the name starting at `start` of names each ended by a nul.
fn named(names: &[u8], start: usize) -> &CStr {
    CStr::from_bytes_until_nul(&names[start..]).unwrap_or_default()
}

/// `place` opened and listed through `listing` into `buffers`, with its first [`RUN`] files
/// looked at: each entry with its kind, `.` and `..` left out, as `fs::read_dir` lists, an entry
/// whose kind cannot be told left out. none when it will not open, as a folder replaced by a link
/// or a file since its parent was listed.
fn list_into(
    place: Place,
    listing: &mut [MaybeUninit<u8>],
    buffers: &mut Buffers,
) -> Option<OwnedFd> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    let dir = match place {
        Place::Start(path) => rustix::fs::open(&path, flags, Mode::empty()),
        Place::Below(parent, name) => {
            rustix::fs::openat(&*parent, &name, flags | OFlags::NOFOLLOW, Mode::empty())
        }
    }
    .ok()?;
    let mut files = 0;
    let mut entries = RawDir::new(&dir, listing);
    while let Some(Ok(entry)) = entries.next() {
        let name = entry.file_name();
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        let kind = match entry.file_type() {
            FileType::Unknown => match rustix::fs::statat(&dir, name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) => FileType::from_raw_mode(stat.st_mode),
                Err(_) => continue,
            },
            kind => kind,
        };
        let looked = (kind == FileType::RegularFile && files < RUN).then(|| {
            files += 1;
            looked_at(&dir, name)
        });
        buffers.entries.push((buffers.names.len(), kind, looked));
        buffers.names.extend_from_slice(name.to_bytes_with_nul());
    }
    Some(dir)
}

fn looked_at(dir: &OwnedFd, name: &CStr) -> Looked {
    let stat = rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW).ok()?;
    Some(of(&stat))
}

/// a buffer for listings, made by this thread once for each lister.
fn listing_buffer() -> Vec<MaybeUninit<u8>> {
    vec![MaybeUninit::uninit(); LISTING_BUFFER]
}

/// a folder on the walk's stack: not asked for yet, asked of a helper, or listed.
enum Slot {
    Waiting(Place),
    Asked,
    Listed(Option<OwnedFd>, Buffers),
}

/// what the walk and its helpers share: jobs waiting, jobs done, whether the walk is done
/// handing out jobs, and who waits, so a job or a result wakes someone only when someone waits.
#[derive(Default)]
struct Queue {
    jobs: std::collections::VecDeque<Job>,
    done: std::collections::VecDeque<Done>,
    closed: bool,
    idle: usize,
    walk_waits: bool,
}

/// the queue, a wake for helpers and a wake for the walk.
type Shared = (
    std::sync::Mutex<Queue>,
    std::sync::Condvar,
    std::sync::Condvar,
);

/// what the walk from `starts` finds, into `scanned` and `others`, as `walk_by_paths` gathers
/// it, on the run's threads once it is large: helpers for the whole walk list the folders the
/// walk reaches next and look at the files of each folder in runs, into buffers this thread makes
/// and takes back, while this thread keeps the walk's order, makes every path and records every
/// look, and does jobs itself while it waits. false when `stopped` ended it.
pub(super) fn walk(
    starts: Vec<(PathBuf, String)>,
    others: &mut HashMap<String, Entry>,
    stopped: &dyn Fn() -> bool,
    threads: Threads,
    scanned: &mut Scanned,
) -> bool {
    let most = threads.limit(
        std::thread::available_parallelism()
            .map_or(1, std::num::NonZero::get)
            .min(HELPERS),
    );
    let queue: Shared = Default::default();
    std::thread::scope(|scope| {
        let mut walk = Walk::new(&queue);
        let mut start_helper = || {
            let queue = &queue;
            let mut listing = listing_buffer();
            crate::threads::scoped(scope, move || {
                while let Some(job) = take(queue) {
                    let done = run(job, &mut listing);
                    let (lock, _, walk_wakes) = queue;
                    let mut held = lock
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    held.done.push_back(done);
                    let waits = held.walk_waits;
                    drop(held);
                    if waits {
                        walk_wakes.notify_one();
                    }
                }
            })
        };
        let walked = walk.walk(starts, others, stopped, scanned, most, &mut start_helper);
        walk.close();
        for helper in std::mem::take(&mut walk.started) {
            helper.join();
        }
        walk.finish(scanned);
        walked
    })
}

/// the next job, waiting for one until the queue closes.
fn take(queue: &Shared) -> Option<Job> {
    let (lock, helper_wakes, _) = queue;
    let mut held = lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    loop {
        if let Some(job) = held.jobs.pop_front() {
            return Some(job);
        }
        if held.closed {
            return None;
        }
        held.idle += 1;
        held = helper_wakes
            .wait(held)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held.idle -= 1;
    }
}

/// a walk in progress: its stack's folders, the jobs out, spare buffers, and what it found that
/// comes out in the walk's own order.
struct Walk<'q, H> {
    queue: &'q Shared,
    started: Vec<H>,
    /// whether helpers took over the jobs, else this thread does each at once.
    helped: bool,
    /// jobs this thread did itself, waiting to be taken in.
    ready: std::collections::VecDeque<Done>,
    slots: HashMap<usize, Slot>,
    /// each batch of looks out, by number: the path of each file and where it is in the walk.
    batches: HashMap<usize, Vec<(String, At)>>,
    /// buffers jobs gave back, emptied for the next.
    spare: Vec<Buffers>,
    /// listings asked of helpers and not walked yet.
    asked: usize,
    /// batches of looks out.
    looking: usize,
    /// the next batch's number.
    next_batch: usize,
    /// the next file's place in the walk's order.
    seq: usize,
    temporaries: Vec<(usize, String)>,
    alone: Vec<(usize, String)>,
    /// for each path that names not utf-8 may share, the place in the walk's order of the look
    /// kept for it, as looks come back out of order.
    shared: HashMap<String, usize>,
    /// this thread's buffer for the listings it makes itself.
    listing: Vec<MaybeUninit<u8>>,
}

impl<'q, H: HelperJoin> Walk<'q, H> {
    fn new(queue: &'q Shared) -> Self {
        Walk {
            queue,
            started: Vec::new(),
            helped: false,
            ready: std::collections::VecDeque::new(),
            slots: HashMap::new(),
            batches: HashMap::new(),
            spare: Vec::new(),
            asked: 0,
            looking: 0,
            next_batch: 0,
            seq: 0,
            temporaries: Vec::new(),
            alone: Vec::new(),
            shared: HashMap::new(),
            listing: listing_buffer(),
        }
    }

    fn walk(
        &mut self,
        starts: Vec<(PathBuf, String)>,
        others: &mut HashMap<String, Entry>,
        stopped: &dyn Fn() -> bool,
        scanned: &mut Scanned,
        most: usize,
        start_helper: &mut impl FnMut() -> H,
    ) -> bool {
        let mut stack: Vec<(usize, String)> = Vec::new();
        let mut next_slot = 0;
        for (path, relative) in starts {
            self.slots
                .insert(next_slot, Slot::Waiting(Place::Start(path)));
            stack.push((next_slot, relative));
            next_slot += 1;
        }
        let mut files = 0;
        while let Some((slot, relative)) = stack.pop() {
            if stopped() {
                return false;
            }
            self.add_helpers(files, stack.len(), most, start_helper);
            self.ask_ahead(&stack);
            let Some((dir, mut buffers)) = self.listing(slot, scanned) else {
                continue;
            };
            let Some(dir) = dir else {
                self.give_back(buffers);
                continue;
            };
            let dir = Arc::new(dir);
            // a path two names not utf-8 can share shows a replacement character
            let folder_shares = relative.contains(char::REPLACEMENT_CHARACTER);
            let mut unlooked: Vec<Unlooked> = Vec::with_capacity(RUN);
            for (start, kind, looked) in buffers.entries.drain(..) {
                let name = named(&buffers.names, start);
                let shown = OsStr::from_bytes(name.to_bytes()).to_string_lossy();
                let path = format!("{relative}/{shown}");
                match kind {
                    FileType::Directory => {
                        others.insert(path.clone(), Entry::Folder);
                        let below = Place::Below(Arc::clone(&dir), name.to_owned());
                        self.slots.insert(next_slot, Slot::Waiting(below));
                        stack.push((next_slot, path));
                        next_slot += 1;
                    }
                    FileType::RegularFile => {
                        files += 1;
                        let at = At {
                            seq: self.seq,
                            temporary: place::is_copy_temporary(&shown),
                            shares: folder_shares
                                || (!name.to_bytes().is_ascii()
                                    && shown.contains(char::REPLACEMENT_CHARACTER)),
                        };
                        self.seq += 1;
                        match looked {
                            Some(looked) => self.record(scanned, path, at, looked),
                            None => unlooked.push((start, path, at)),
                        }
                        // the looks go out a run at a time, so a folder of many files holds a
                        // few runs of paths, not all of them, and helpers join once it is large
                        if unlooked.len() == RUN {
                            self.add_helpers(files, stack.len(), most, start_helper);
                            self.give_looks(&dir, &buffers.names, &mut unlooked, scanned);
                        }
                    }
                    FileType::Symlink => {
                        others.insert(path, Entry::Link);
                    }
                    _ => {
                        others.insert(path, Entry::Special);
                    }
                }
            }
            if !unlooked.is_empty() {
                self.add_helpers(files, stack.len(), most, start_helper);
                self.give_looks(&dir, &buffers.names, &mut unlooked, scanned);
            }
            self.give_back(buffers);
        }
        // every folder asked for was walked, so only looks can be out
        while self.looking > 0 {
            self.take_in(scanned);
        }
        true
    }

    /// hands out the looks at `unlooked`, files of the folder `dir` named in `names`, as one job,
    /// and takes in what came back while too many are out.
    fn give_looks(
        &mut self,
        dir: &Arc<OwnedFd>,
        names: &[u8],
        unlooked: &mut Vec<Unlooked>,
        scanned: &mut Scanned,
    ) {
        let mut look = self.spare.pop().unwrap_or_default();
        let mut paths = Vec::with_capacity(unlooked.len());
        for (start, path, at) in unlooked.drain(..) {
            look.starts.push(look.names.len());
            look.names
                .extend_from_slice(named(names, start).to_bytes_with_nul());
            paths.push((path, at));
        }
        look.looks.reserve(paths.len());
        let batch = self.next_batch;
        self.next_batch += 1;
        self.batches.insert(batch, paths);
        self.looking += 1;
        self.give(Job::Look(batch, Arc::clone(dir), look));
        while self.looking > 2 * self.started.len().max(1) {
            self.take_in(scanned);
        }
    }

    /// starts helpers once the walk is large, one for each [`FILES_A_HELPER`] files it has
    /// listed and each 64 folders waiting, two at first, `most` at most.
    fn add_helpers(
        &mut self,
        files: usize,
        waiting: usize,
        most: usize,
        start_helper: &mut impl FnMut() -> H,
    ) {
        if most < 2 || (!self.helped && waiting < HELPED_FROM_FOLDERS && files < HELPED_FROM_FILES)
        {
            return;
        }
        let wanted = most.min((files / FILES_A_HELPER + waiting / 64).max(2));
        while self.started.len() < wanted {
            self.started.push(start_helper());
            // with no helper started, this thread goes on alone
            self.helped = self.started.iter().any(HelperJoin::is_threaded);
        }
    }

    /// asks helpers to list the folders the walk reaches next, keeping at most one a helper out
    /// or listed and not yet walked.
    fn ask_ahead(&mut self, stack: &[(usize, String)]) {
        if !self.helped {
            return;
        }
        let ahead = self.started.len();
        for (slot, _) in stack.iter().rev().take(ahead) {
            if self.asked >= ahead {
                return;
            }
            if let Some(Slot::Waiting(_)) = self.slots.get(slot) {
                let Some(Slot::Waiting(place)) = self.slots.insert(*slot, Slot::Asked) else {
                    continue;
                };
                self.asked += 1;
                let buffers = self.spare.pop().unwrap_or_default();
                self.give(Job::List(*slot, place, buffers));
            }
        }
    }

    /// the listing of `slot`'s folder: listed here at once when no helper has it, else waited
    /// for, doing other jobs meanwhile.
    fn listing(
        &mut self,
        slot: usize,
        scanned: &mut Scanned,
    ) -> Option<(Option<OwnedFd>, Buffers)> {
        loop {
            match self.slots.remove(&slot) {
                Some(Slot::Waiting(place)) => {
                    let mut buffers = self.spare.pop().unwrap_or_default();
                    let dir = list_into(place, &mut self.listing, &mut buffers);
                    return Some((dir, buffers));
                }
                Some(Slot::Listed(dir, buffers)) => {
                    self.asked -= 1;
                    return Some((dir, buffers));
                }
                Some(Slot::Asked) => {
                    self.slots.insert(slot, Slot::Asked);
                    self.take_in(scanned);
                }
                None => return None,
            }
        }
    }

    /// keeps `buffers` emptied for the next job.
    fn give_back(&mut self, mut buffers: Buffers) {
        buffers.names.clear();
        buffers.entries.clear();
        buffers.starts.clear();
        buffers.looks.clear();
        self.spare.push(buffers);
    }

    /// hands `job` to the helpers, or does it now when there are none.
    fn give(&mut self, job: Job) {
        if !self.helped {
            let done = run(job, &mut self.listing);
            self.ready.push_back(done);
            return;
        }
        let (lock, helper_wakes, _) = self.queue;
        let mut held = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held.jobs.push_back(job);
        let idle = held.idle > 0;
        drop(held);
        if idle {
            helper_wakes.notify_one();
        }
    }

    /// takes in one finished job: one this thread did, one a helper did, or, when none is done,
    /// a waiting job this thread does itself, else it waits for a helper's.
    fn take_in(&mut self, scanned: &mut Scanned) {
        let done = if let Some(done) = self.ready.pop_front() {
            done
        } else {
            let (lock, _, walk_wakes) = self.queue;
            let mut held = lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            loop {
                if let Some(done) = held.done.pop_front() {
                    break done;
                }
                if let Some(job) = held.jobs.pop_front() {
                    drop(held);
                    break run(job, &mut self.listing);
                }
                held.walk_waits = true;
                held = walk_wakes
                    .wait(held)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                held.walk_waits = false;
            }
        };
        match done {
            Done::Listed(slot, dir, buffers) => {
                self.slots.insert(slot, Slot::Listed(dir, buffers));
            }
            Done::Looked(batch, mut buffers) => {
                self.looking -= 1;
                let paths = self.batches.remove(&batch).unwrap_or_default();
                let looks = std::mem::take(&mut buffers.looks);
                for ((path, at), looked) in paths.into_iter().zip(looks.iter().copied()) {
                    self.record(scanned, path, at, looked);
                }
                buffers.looks = looks;
                self.give_back(buffers);
            }
        }
    }

    fn record(&mut self, scanned: &mut Scanned, path: String, at: At, looked: Looked) {
        if let Some((look, writable, alone)) = looked {
            if at.temporary {
                self.temporaries.push((at.seq, path.clone()));
            }
            if writable {
                scanned.writable.insert(path.clone());
            }
            if alone {
                self.alone.push((at.seq, path.clone()));
            }
            if !at.shares || later_than_kept(&mut self.shared, &path, at.seq) {
                scanned.found.insert(path, look);
            }
        }
    }

    /// ends the helpers' work: no job is handed out after, and none left waiting is done.
    fn close(&mut self) {
        let (lock, helper_wakes, _) = self.queue;
        let mut held = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held.closed = true;
        held.jobs.clear();
        drop(held);
        helper_wakes.notify_all();
    }

    /// what came out of order, put in the walk's order.
    fn finish(&mut self, scanned: &mut Scanned) {
        self.temporaries.sort_unstable();
        self.alone.sort_unstable();
        scanned
            .temporaries
            .extend(self.temporaries.drain(..).map(|(_, path)| path));
        for (_, path) in self.alone.drain(..) {
            push_alone(&mut scanned.alone, &path);
        }
    }
}

/// a file's place in the walk: its place in the walk's order, whether placement stages files
/// under its name, and whether its path may be another name's too.
#[derive(Clone, Copy)]
struct At {
    seq: usize,
    temporary: bool,
    shares: bool,
}

/// a file of a listed folder whose look is still to be had: where its name starts in the
/// listing, its path and its place in the walk.
type Unlooked = (usize, String, At);

/// whether a look at `seq` in the walk's order is the one to keep for `path`, a path two names
/// not utf-8 can share, as they show with a replacement character: a walk in order keeps the
/// later one's look. `shared` holds the place of the look kept for each such path.
fn later_than_kept(shared: &mut HashMap<String, usize>, path: &str, seq: usize) -> bool {
    match shared.get_mut(path) {
        Some(kept) if *kept > seq => false,
        Some(kept) => {
            *kept = seq;
            true
        }
        None => {
            shared.insert(path.to_owned(), seq);
            true
        }
    }
}

/// a helper as [`walk`] starts it: whether it runs on a thread of its own, or its work waits for
/// the walk to end, as when the system refused the thread.
trait HelperJoin {
    fn is_threaded(&self) -> bool;
}

impl<F: FnMut()> HelperJoin for crate::threads::Scoped<'_, F, ()> {
    fn is_threaded(&self) -> bool {
        crate::threads::Scoped::is_threaded(self)
    }
}

/// what `Look::of`, `Permissions::readonly` and `read_only_alone` make of the same file's
/// metadata: its look, whether any may write it, and whether it is read-only with no other name.
fn of(stat: &Stat) -> (Look, bool, bool) {
    let readonly = stat.st_mode & 0o222 == 0;
    let look = Look {
        size: stat.st_size.cast_unsigned(),
        modified: since_1970(stat.st_mtime, stat.st_mtime_nsec),
    };
    (look, !readonly, readonly && stat.st_nlink == 1)
}

/// nanoseconds since 1970 of a time a stat gives, as `units::nanos` makes them of the time std
/// reads from the same fields: none before 1970 or of a nanosecond count out of range, at most
/// `u64::MAX`.
fn since_1970(seconds: i64, nanoseconds: impl TryInto<i64>) -> u64 {
    let Ok(nanoseconds) = nanoseconds.try_into() else {
        return 0;
    };
    if seconds < 0 || !(0..1_000_000_000).contains(&nanoseconds) {
        return 0;
    }
    let total = i128::from(seconds) * 1_000_000_000 + i128::from(nanoseconds);
    u64::try_from(total).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::since_1970;

    /// looks that come back out of order keep, for a path two names not utf-8 share, the look
    /// of the later name in the walk's order, as a walk in order does, whichever came back first.
    #[test]
    fn looks_back_out_of_order_keep_the_later_name() {
        struct Inline;
        impl super::HelperJoin for Inline {
            fn is_threaded(&self) -> bool {
                false
            }
        }
        let queue = super::Shared::default();
        let look = |size| {
            Some((
                crate::project::state::Look { size, modified: 0 },
                false,
                false,
            ))
        };
        let shared = "data/odd-\u{fffd}.bin";
        for order in [[(3, 30), (8, 80)], [(8, 80), (3, 30)]] {
            let mut walk = super::Walk::<Inline>::new(&queue);
            let mut scanned = super::Scanned::default();
            for (seq, size) in order {
                let at = |shares| super::At {
                    seq,
                    temporary: false,
                    shares,
                };
                walk.record(&mut scanned, shared.to_owned(), at(true), look(size));
                walk.record(
                    &mut scanned,
                    format!("data/{seq}.bin"),
                    at(false),
                    look(size),
                );
            }
            assert_eq!(scanned.found[shared].size, 80, "{order:?}");
            assert_eq!(scanned.found.len(), 3, "{order:?}");
        }
    }

    /// a stat's time reads as std's metadata reads it: none before 1970, none for nanoseconds out
    /// of range, the sum otherwise, the most a u64 holds past it.
    #[test]
    fn a_stat_time_reads_as_std_reads_it() {
        assert_eq!(since_1970(0, 0_i64), 0);
        assert_eq!(since_1970(1, 5_i64), 1_000_000_005);
        assert_eq!(since_1970(-1, 999_999_999_i64), 0);
        assert_eq!(since_1970(5, 1_000_000_000_i64), 0);
        assert_eq!(since_1970(5, -1_i64), 0);
        assert_eq!(since_1970(i64::MAX, 0_i64), u64::MAX);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        assert_eq!(
            since_1970(
                now.as_secs().try_into().unwrap(),
                i64::from(now.subsec_nanos())
            ),
            crate::units::nanos(std::time::UNIX_EPOCH + now)
        );
    }
}
