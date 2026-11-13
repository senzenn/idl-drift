//! Count the decode misses of a live Solana parser per program, and alert at the first bad
//! data after a program upgrade.
//!
//! A [`Guard`] counts each update that a parser or decoder sees, by [`Kind`] and
//! [`Outcome`]. With [`Guard::from_idl`], it also knows the discriminators and the fixed
//! lengths of the IDL, so it sees what a decoder accepts in silence: extra bytes at the end of
//! a grown account, or data that is shorter than the type.
//!
//! [`guard!`] wraps a parser or decoder. The wrapper returns each value unchanged:
//!
//! ```ignore
//! use std::sync::Arc;
//! use idl_drift_guard::{guard, Guard, Window};
//!
//! let pump = Arc::new(
//!     Guard::from_idl(include_str!("../idls/pump.json"))?
//!         .alert_after(5, Window::Time(std::time::Duration::from_secs(60)), |alert| {
//!             eprintln!("{alert}");
//!         }),
//! );
//! // Shipstern 0.12 (`vixen` for Vixen 0.7), in place of the parser:
//! let parser = guard!(shipstern, pump::AccountParser, pump.clone());
//! // Carbon 2.0, in place of the decoder:
//! let decoder = guard!(carbon, PumpfunDecoder, pump.clone());
//! // Prometheus text for a `/metrics` endpoint:
//! let text = idl_drift_guard::prometheus([&*pump]);
//! ```
//!
//! Use one guard per program. The alert callback must not panic or block.
//!
//! The macro names the framework crates, so the calling crate needs them as direct
//! dependencies: `shipstern-core` (or `yellowstone-vixen-core`), or `carbon-core`,
//! `solana-account` and `solana-instruction`. This crate has no framework dependency.

mod macros;
mod shapes;

use std::fmt::{self, Write as _};
use std::sync::atomic::{
    AtomicBool, AtomicU64,
    Ordering::{Acquire, Relaxed, Release},
};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use idl_drift::model::Idl;

use shapes::{Lookup, Shape, Shapes};

/// What an update holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Kind {
    Account,
    Instruction,
    /// A self-CPI event: instruction data that starts with an event envelope of the IDL.
    Event,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Account, Kind::Instruction, Kind::Event];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Account => "account",
            Kind::Instruction => "instruction",
            Kind::Event => "event",
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

/// How the guard counts one update.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Outcome {
    /// The decoder returned a value, and the length fits the IDL (or the IDL fixes none).
    Decoded,
    /// The update is for another program, or the parser skipped it.
    Filtered,
    /// No type of the IDL (or of the decoder) has this discriminator.
    UnknownDiscriminator,
    /// The decoder failed on data whose discriminator and length fit a type.
    DecodeError,
    /// The decoder failed, and the data is shorter than the type.
    ShortData,
    /// The decoder returned a value, but bytes follow the type. Borsh decoders accept them in
    /// silence, so a grown account shows up here.
    ExtraBytes,
    /// The decoder returned a value from data shorter than the type, or no type has this
    /// length where the IDL selects types by length.
    SizeMismatch,
}

impl Outcome {
    pub const ALL: [Outcome; 7] = [
        Outcome::Decoded,
        Outcome::Filtered,
        Outcome::UnknownDiscriminator,
        Outcome::DecodeError,
        Outcome::ShortData,
        Outcome::ExtraBytes,
        Outcome::SizeMismatch,
    ];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Decoded => "decoded",
            Outcome::Filtered => "filtered",
            Outcome::UnknownDiscriminator => "unknown_discriminator",
            Outcome::DecodeError => "decode_error",
            Outcome::ShortData => "short_data",
            Outcome::ExtraBytes => "extra_bytes",
            Outcome::SizeMismatch => "size_mismatch",
        }
    }

    /// Each outcome except `Decoded` and `Filtered`.
    #[must_use]
    pub fn is_miss(self) -> bool {
        !matches!(self, Outcome::Decoded | Outcome::Filtered)
    }

    fn index(self) -> usize {
        self as usize
    }
}

/// What the decoder said about an update. [`guard!`] gets it from the framework's result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Decoded,
    /// The decoder skipped the update.
    Filtered,
    /// The decoder found no type for the discriminator.
    UnknownDiscriminator,
    /// The decoder failed for another reason, or did not say why (Carbon's `None`).
    Failed,
}

/// The bytes of one update, and where they came from.
#[derive(Debug, Clone, Copy, Default)]
pub struct Update<'a> {
    /// The owner of the account, or the program of the instruction. An update of another
    /// program counts as filtered.
    pub program: Option<&'a [u8]>,
    pub data: &'a [u8],
    pub slot: Option<u64>,
    /// The account address, or the signature of the instruction's transaction.
    pub key: Option<&'a [u8]>,
}

/// The span in which [`Guard::alert_after`] counts misses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    /// From the first miss, for this long.
    Time(Duration),
    /// From the first miss, for this many updates that are not filtered.
    Updates(u64),
}

/// The first miss of an alert window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Miss {
    pub kind: Kind,
    pub outcome: Outcome,
    /// The IDL type that the discriminator selects.
    pub type_name: Option<String>,
    pub len: usize,
    /// The length that the IDL fixes for that type.
    pub expected_len: Option<usize>,
    /// Up to the first 8 bytes of the data.
    pub head: Vec<u8>,
    /// `None` for Carbon: its decoders do not see the slot.
    pub slot: Option<u64>,
    /// The account address or the transaction signature. `None` for Carbon.
    pub key: Option<Vec<u8>>,
}

impl fmt::Display for Miss {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.kind.as_str(), self.outcome.as_str())?;
        if let Some(name) = &self.type_name {
            write!(f, " ({name})")?;
        }
        write!(f, ": {} bytes", self.len)?;
        if let Some(expected) = self.expected_len {
            write!(f, ", the IDL fixes {expected}")?;
        }
        write!(f, ", starts with 0x")?;
        for byte in &self.head {
            write!(f, "{byte:02x}")?;
        }
        if let Some(slot) = self.slot {
            write!(f, ", slot {slot}")?;
        }
        if let Some(key) = &self.key {
            write!(f, ", {}", bs58::encode(key).into_string())?;
        }
        Ok(())
    }
}

/// Raised once per window, when the window holds the given number of misses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alert {
    /// The program id in base58.
    pub program: String,
    pub name: String,
    /// The misses in the window, the one that raised the alert included.
    pub misses: u64,
    pub first: Miss,
}

impl fmt::Display for Alert {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {}: {} decode misses; first: {}",
            self.name, self.program, self.misses, self.first
        )
    }
}

/// The counts of one kind of update.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Counts {
    pub decoded: u64,
    pub filtered: u64,
    pub unknown_discriminator: u64,
    pub decode_error: u64,
    pub short_data: u64,
    pub extra_bytes: u64,
    pub size_mismatch: u64,
}

impl Counts {
    #[must_use]
    pub fn get(&self, outcome: Outcome) -> u64 {
        match outcome {
            Outcome::Decoded => self.decoded,
            Outcome::Filtered => self.filtered,
            Outcome::UnknownDiscriminator => self.unknown_discriminator,
            Outcome::DecodeError => self.decode_error,
            Outcome::ShortData => self.short_data,
            Outcome::ExtraBytes => self.extra_bytes,
            Outcome::SizeMismatch => self.size_mismatch,
        }
    }

    #[must_use]
    pub fn misses(&self) -> u64 {
        Outcome::ALL
            .iter()
            .filter(|o| o.is_miss())
            .map(|o| self.get(*o))
            .sum()
    }
}

/// The counts of one guard at one time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// The program id in base58.
    pub program: String,
    pub name: String,
    pub accounts: Counts,
    pub instructions: Counts,
    pub events: Counts,
    pub alerts: u64,
}

impl Snapshot {
    #[must_use]
    pub fn get(&self, kind: Kind) -> &Counts {
        match kind {
            Kind::Account => &self.accounts,
            Kind::Instruction => &self.instructions,
            Kind::Event => &self.events,
        }
    }

    #[must_use]
    pub fn misses(&self) -> u64 {
        Kind::ALL.iter().map(|k| self.get(*k).misses()).sum()
    }
}

#[derive(Debug)]
#[non_exhaustive]
pub enum GuardError {
    /// The IDL does not parse.
    Idl(String),
    /// The IDL's address is not a 32-byte base58 key.
    Address(String),
    /// [`Guard::expect_len`] names a type that the IDL does not select.
    NoSuchType { kind: Kind, name: String },
}

impl fmt::Display for GuardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GuardError::Idl(e) => write!(f, "the IDL does not parse: {e}"),
            GuardError::Address(address) => write!(
                f,
                "the IDL address {address:?} is not a 32-byte base58 key; use Guard::new with the program id"
            ),
            GuardError::NoSuchType { kind, name } => write!(
                f,
                "the IDL has no {} type {name:?} with a discriminator or a length",
                kind.as_str()
            ),
        }
    }
}

impl std::error::Error for GuardError {}

type Callback = Arc<dyn Fn(&Alert) + Send + Sync>;

struct Rule {
    misses: u64,
    window: Window,
    callback: Callback,
}

struct Open {
    /// Where the window starts: nanoseconds after the guard was made, or the index of the
    /// first miss among the updates that are not filtered.
    start: u64,
    misses: u64,
    first: Miss,
    fired: bool,
}

/// The counters of one program. Share it with `Arc`: each method takes `&self`, and the
/// counters are atomics, so a decoded update costs a discriminator lookup and one add. Use one
/// guard per program; [`prometheus`] sums guards with the same program and name.
pub struct Guard {
    program: [u8; 32],
    program_text: String,
    name: String,
    shapes: Shapes,
    counts: [[AtomicU64; 7]; 3],
    alerts: AtomicU64,
    rule: Option<Rule>,
    window: Mutex<Option<Open>>,
    created: Instant,
    /// A copy of the open window's `fired` and `start`, so that a miss in a window that has
    /// already alerted returns without the lock.
    fired: AtomicBool,
    fired_start: AtomicU64,
    mismatch: OnceLock<[u8; 32]>,
}

impl fmt::Debug for Guard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Guard")
            .field("program", &self.program_text)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl Guard {
    /// A guard with no IDL: it counts what the decoder says, and cannot see extra bytes.
    #[must_use]
    pub fn new(program: [u8; 32], name: &str) -> Self {
        Self {
            program,
            program_text: bs58::encode(program).into_string(),
            name: name.to_string(),
            shapes: Shapes::default(),
            counts: Default::default(),
            alerts: AtomicU64::new(0),
            rule: None,
            window: Mutex::new(None),
            created: Instant::now(),
            fired: AtomicBool::new(false),
            fired_start: AtomicU64::new(0),
            mismatch: OnceLock::new(),
        }
    }

    /// A guard for the program at the IDL's address, with the discriminators and fixed
    /// lengths of its accounts, instructions and self-CPI events.
    ///
    /// # Errors
    ///
    /// When the IDL does not parse, or its address is not a base58 key.
    pub fn from_idl(json: &str) -> Result<Self, GuardError> {
        let idl = Idl::from_json(json).map_err(|e| GuardError::Idl(e.to_string()))?;
        let program = bs58::decode(&idl.address)
            .into_vec()
            .ok()
            .and_then(|key| <[u8; 32]>::try_from(key).ok())
            .ok_or_else(|| GuardError::Address(idl.address.clone()))?;
        Ok(Self::with_idl(program, &idl))
    }

    /// [`Guard::from_idl`] for the program `program`, whatever the IDL's address is. For an
    /// IDL with no address, or a deploy at another address (devnet, a fork).
    ///
    /// # Errors
    ///
    /// When the IDL does not parse.
    pub fn from_idl_for(program: [u8; 32], json: &str) -> Result<Self, GuardError> {
        let idl = Idl::from_json(json).map_err(|e| GuardError::Idl(e.to_string()))?;
        Ok(Self::with_idl(program, &idl))
    }

    fn with_idl(program: [u8; 32], idl: &Idl) -> Self {
        let mut guard = Self::new(program, &idl.metadata.name);
        guard.shapes = Shapes::from_idl(idl);
        guard
    }

    /// Expect exactly `len` bytes for a type, in place of the IDL's size. For example, an
    /// account that the program allocates with spare bytes.
    ///
    /// # Errors
    ///
    /// When the IDL has no such type with a discriminator or a length.
    pub fn expect_len(self, kind: Kind, name: &str, len: usize) -> Result<Self, GuardError> {
        self.expect(kind, name, len, false)
    }

    /// Expect at least `len` bytes for a type: longer data is not `extra_bytes`. For example,
    /// an instruction whose clients may leave out a last argument.
    ///
    /// # Errors
    ///
    /// When the IDL has no such type with a discriminator or a length.
    pub fn expect_min_len(self, kind: Kind, name: &str, len: usize) -> Result<Self, GuardError> {
        self.expect(kind, name, len, true)
    }

    fn expect(
        mut self,
        kind: Kind,
        name: &str,
        len: usize,
        at_least: bool,
    ) -> Result<Self, GuardError> {
        if self.shapes.table_mut(kind).expect_len(name, len, at_least) {
            Ok(self)
        } else {
            Err(GuardError::NoSuchType {
                kind,
                name: name.to_string(),
            })
        }
    }

    /// Call `callback` when one window holds `misses` misses (at least 1), with the first
    /// miss of the window. One alert per window at most.
    ///
    /// The callback runs on the decoder's thread, inside the decode call. It must not panic
    /// and must not block: send the alert to a channel, and act on it in another task.
    #[must_use]
    pub fn alert_after(
        mut self,
        misses: u64,
        window: Window,
        callback: impl Fn(&Alert) + Send + Sync + 'static,
    ) -> Self {
        self.rule = Some(Rule {
            misses: misses.max(1),
            window,
            callback: Arc::new(callback),
        });
        self
    }

    #[must_use]
    pub fn program(&self) -> &[u8; 32] {
        &self.program
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// `true` when `program` is the guard's program. Else the guard keeps the first such
    /// program for [`Guard::program_mismatch`] and prints one warning to stderr: with a wrong
    /// program id, each update counts as filtered.
    pub fn check_program(&self, program: &[u8]) -> bool {
        if program == self.program {
            return true;
        }
        if let Ok(other) = <[u8; 32]>::try_from(program) {
            if self.mismatch.set(other).is_ok() {
                eprintln!(
                    "idl-drift-guard: the guard {} is for program {}, but its decoder is for program {}; each update counts as filtered",
                    self.name,
                    self.program_text,
                    bs58::encode(other).into_string()
                );
            }
        }
        false
    }

    /// The first other program that the decoder is for: the parser's `program_id()`, or a
    /// program for which the decoder returned a value.
    #[must_use]
    pub fn program_mismatch(&self) -> Option<&[u8; 32]> {
        self.mismatch.get()
    }

    /// Each IDL type of `kind` that the guard can select, with the length the IDL fixes.
    pub fn shapes(&self, kind: Kind) -> impl Iterator<Item = (&str, Option<usize>)> {
        self.shapes
            .table(kind)
            .iter()
            .map(|shape| (shape.name.as_str(), shape.len))
    }

    /// Count one update. [`guard!`] calls this after each decode.
    pub fn observe(&self, kind: Kind, verdict: Verdict, update: &Update<'_>) -> Outcome {
        self.observe_at(kind, verdict, update, None)
    }

    fn observe_at(
        &self,
        kind: Kind,
        verdict: Verdict,
        update: &Update<'_>,
        now: Option<Instant>,
    ) -> Outcome {
        // Most updates of a pipeline are for other programs; they need no lookup.
        if let Some(program) = update.program.filter(|p| *p != self.program) {
            if verdict == Verdict::Decoded {
                self.check_program(program);
            }
            return self.count(kind, Outcome::Filtered);
        }
        let kind = if kind == Kind::Instruction && self.shapes.is_event(update.data) {
            Kind::Event
        } else {
            kind
        };
        if verdict == Verdict::Filtered {
            return self.count(kind, Outcome::Filtered);
        }
        let (outcome, shape) = classify(
            verdict,
            self.shapes.table(kind).lookup(update.data),
            update.data.len(),
        );
        self.count(kind, outcome);
        if outcome.is_miss() {
            if let Some(rule) = &self.rule {
                self.on_miss(rule, kind, outcome, shape, update, now);
            }
        }
        outcome
    }

    fn count(&self, kind: Kind, outcome: Outcome) -> Outcome {
        self.counts[kind.index()][outcome.index()].fetch_add(1, Relaxed);
        outcome
    }

    fn on_miss(
        &self,
        rule: &Rule,
        kind: Kind,
        outcome: Outcome,
        shape: Option<&Shape>,
        update: &Update<'_>,
        now: Option<Instant>,
    ) {
        // Each window reads only its own measure: the clock, or the update counters.
        let (at, span) = match rule.window {
            Window::Time(span) => {
                let now = now.unwrap_or_else(Instant::now);
                (
                    nanos(now.saturating_duration_since(self.created)),
                    nanos(span),
                )
            }
            Window::Updates(n) => (self.unfiltered().saturating_sub(1), n),
        };
        if self.fired.load(Acquire) && at.saturating_sub(self.fired_start.load(Relaxed)) < span {
            return;
        }
        let alert = {
            let mut window = self.window.lock().unwrap_or_else(PoisonError::into_inner);
            let expired = window
                .as_ref()
                .is_none_or(|open| at.saturating_sub(open.start) >= span);
            let open = if expired {
                self.fired.store(false, Relaxed);
                window.insert(Open {
                    start: at,
                    misses: 0,
                    first: miss(kind, outcome, shape, update),
                    fired: false,
                })
            } else {
                match window.as_mut() {
                    Some(open) => open,
                    None => return,
                }
            };
            open.misses += 1;
            (open.misses >= rule.misses && !open.fired).then(|| {
                open.fired = true;
                self.fired_start.store(open.start, Relaxed);
                self.fired.store(true, Release);
                Alert {
                    program: self.program_text.clone(),
                    name: self.name.clone(),
                    misses: open.misses,
                    first: open.first.clone(),
                }
            })
        };
        // The callback runs after the lock is free, so it may read the guard.
        if let Some(alert) = alert {
            self.alerts.fetch_add(1, Relaxed);
            (rule.callback)(&alert);
        }
    }

    fn unfiltered(&self) -> u64 {
        self.counts
            .iter()
            .flat_map(|row| {
                Outcome::ALL
                    .iter()
                    .filter(|o| **o != Outcome::Filtered)
                    .map(|o| row[o.index()].load(Relaxed))
            })
            .sum()
    }

    fn counts(&self, kind: Kind) -> Counts {
        let row = &self.counts[kind.index()];
        let get = |o: Outcome| row[o.index()].load(Relaxed);
        Counts {
            decoded: get(Outcome::Decoded),
            filtered: get(Outcome::Filtered),
            unknown_discriminator: get(Outcome::UnknownDiscriminator),
            decode_error: get(Outcome::DecodeError),
            short_data: get(Outcome::ShortData),
            extra_bytes: get(Outcome::ExtraBytes),
            size_mismatch: get(Outcome::SizeMismatch),
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            program: self.program_text.clone(),
            name: self.name.clone(),
            accounts: self.counts(Kind::Account),
            instructions: self.counts(Kind::Instruction),
            events: self.counts(Kind::Event),
            alerts: self.alerts.load(Relaxed),
        }
    }
}

fn nanos(span: Duration) -> u64 {
    u64::try_from(span.as_nanos()).unwrap_or(u64::MAX)
}

fn classify(verdict: Verdict, lookup: Lookup<'_>, len: usize) -> (Outcome, Option<&Shape>) {
    match (verdict, lookup) {
        (Verdict::Filtered, _) => (Outcome::Filtered, None),
        (Verdict::Decoded, Lookup::Type(shape)) => {
            let outcome = match shape.len {
                Some(fixed) if len > fixed && !shape.at_least => Outcome::ExtraBytes,
                Some(fixed) if len < fixed => Outcome::SizeMismatch,
                _ => Outcome::Decoded,
            };
            (outcome, Some(shape))
        }
        // The decoder knows a type that the IDL does not.
        (Verdict::Decoded, _) => (Outcome::Decoded, None),
        (Verdict::UnknownDiscriminator, Lookup::Type(shape)) => {
            (Outcome::UnknownDiscriminator, Some(shape))
        }
        (Verdict::Failed, Lookup::Type(shape)) => {
            let outcome = if len < shape.len.unwrap_or(shape.payload_start) {
                Outcome::ShortData
            } else {
                Outcome::DecodeError
            };
            (outcome, Some(shape))
        }
        (_, Lookup::WrongLen) => (Outcome::SizeMismatch, None),
        (_, Lookup::Short) => (Outcome::ShortData, None),
        (_, Lookup::Unknown) | (Verdict::UnknownDiscriminator, Lookup::NoIdl) => {
            (Outcome::UnknownDiscriminator, None)
        }
        (Verdict::Failed, Lookup::NoIdl) => (Outcome::DecodeError, None),
    }
}

fn miss(kind: Kind, outcome: Outcome, shape: Option<&Shape>, update: &Update<'_>) -> Miss {
    Miss {
        kind,
        outcome,
        type_name: shape.map(|s| s.name.clone()),
        len: update.data.len(),
        expected_len: shape.and_then(|s| s.len),
        head: update.data.iter().take(8).copied().collect(),
        slot: update.slot,
        key: update.key.map(<[u8]>::to_vec),
    }
}

/// Prometheus text exposition (format 0.0.4) of the counters of each guard. A guard given
/// twice counts once; guards with the same program and name are summed, because a scrape with
/// a series twice fails.
#[must_use]
pub fn prometheus<'a>(guards: impl IntoIterator<Item = &'a Guard>) -> String {
    let mut unique: Vec<&Guard> = Vec::new();
    for guard in guards {
        if !unique.iter().any(|seen| std::ptr::eq(*seen, guard)) {
            unique.push(guard);
        }
    }
    // Labels, counts by kind and outcome, and alerts, in the order of the first guard.
    let mut series: Vec<(String, [[u64; 7]; 3], u64)> = Vec::new();
    for guard in unique {
        let labels = guard.labels();
        if !series.iter().any(|(l, _, _)| *l == labels) {
            series.push((labels.clone(), [[0; 7]; 3], 0));
        }
        let Some((_, counts, alerts)) = series.iter_mut().find(|(l, _, _)| *l == labels) else {
            continue;
        };
        for (row, atomics) in counts.iter_mut().zip(&guard.counts) {
            for (sum, atomic) in row.iter_mut().zip(atomics) {
                *sum += atomic.load(Relaxed);
            }
        }
        *alerts += guard.alerts.load(Relaxed);
    }
    let mut out = String::new();
    // Writing to a String does not fail.
    let _ = writeln!(
        out,
        "# HELP idl_drift_guard_updates_total Updates that a guarded parser or decoder saw, by kind and outcome.\n\
         # TYPE idl_drift_guard_updates_total counter"
    );
    for (labels, counts, _) in &series {
        for kind in Kind::ALL {
            for outcome in Outcome::ALL {
                let _ = writeln!(
                    out,
                    "idl_drift_guard_updates_total{{{labels},kind=\"{}\",outcome=\"{}\"}} {}",
                    kind.as_str(),
                    outcome.as_str(),
                    counts[kind.index()][outcome.index()]
                );
            }
        }
    }
    let _ = writeln!(
        out,
        "# HELP idl_drift_guard_alerts_total Alerts that the guard raised.\n\
         # TYPE idl_drift_guard_alerts_total counter"
    );
    for (labels, _, alerts) in &series {
        let _ = writeln!(out, "idl_drift_guard_alerts_total{{{labels}}} {alerts}");
    }
    out
}

impl Guard {
    fn labels(&self) -> String {
        format!(
            "program=\"{}\",name=\"{}\"",
            escape(&self.program_text),
            escape(&self.name)
        )
    }
}

/// A label value escapes the backslash, the double quote and the line feed.
fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    const PROGRAM: [u8; 32] = [7; 32];

    fn update(data: &[u8]) -> Update<'_> {
        Update {
            program: Some(&PROGRAM),
            data,
            slot: Some(42),
            key: Some(&[1; 32]),
        }
    }

    #[test]
    fn without_an_idl_the_verdict_sets_the_outcome() {
        let guard = Guard::new(PROGRAM, "p");
        let data = [1, 2, 3];
        let cases = [
            (Verdict::Decoded, Outcome::Decoded),
            (Verdict::Filtered, Outcome::Filtered),
            (Verdict::UnknownDiscriminator, Outcome::UnknownDiscriminator),
            (Verdict::Failed, Outcome::DecodeError),
        ];
        for (verdict, outcome) in cases {
            assert_eq!(
                guard.observe(Kind::Account, verdict, &update(&data)),
                outcome
            );
        }
        let foreign = Update {
            program: Some(&[8; 32]),
            ..update(&data)
        };
        assert_eq!(
            guard.observe(Kind::Instruction, Verdict::Decoded, &foreign),
            Outcome::Filtered
        );
        let snapshot = guard.snapshot();
        assert_eq!(
            snapshot.accounts,
            Counts {
                decoded: 1,
                filtered: 1,
                unknown_discriminator: 1,
                decode_error: 1,
                ..Counts::default()
            }
        );
        assert_eq!(snapshot.instructions.filtered, 1);
        assert_eq!(snapshot.misses(), 2);
    }

    fn alerting(misses: u64, window: Window) -> (Guard, Arc<StdMutex<Vec<Alert>>>) {
        let alerts = Arc::new(StdMutex::new(Vec::new()));
        let sink = alerts.clone();
        let guard = Guard::new(PROGRAM, "p").alert_after(misses, window, move |alert| {
            sink.lock().unwrap().push(alert.clone());
        });
        (guard, alerts)
    }

    #[test]
    fn a_count_window_alerts_once_with_the_first_miss() {
        let (guard, alerts) = alerting(3, Window::Updates(5));
        let observe = |verdict, data: &[u8]| guard.observe(Kind::Account, verdict, &update(data));
        observe(Verdict::Failed, &[1]);
        observe(Verdict::Decoded, &[0]);
        observe(Verdict::Failed, &[2]);
        assert!(alerts.lock().unwrap().is_empty());
        // Filtered updates do not move the window.
        observe(Verdict::Filtered, &[0]);
        observe(Verdict::Failed, &[3]);
        observe(Verdict::Failed, &[4]);
        let got = alerts.lock().unwrap().clone();
        assert_eq!(got.len(), 1, "one alert per window: {got:?}");
        assert_eq!(got[0].misses, 3);
        assert_eq!(got[0].first.head, [1]);
        assert_eq!(got[0].first.slot, Some(42));
        assert_eq!(got[0].first.outcome, Outcome::DecodeError);
        assert_eq!(guard.snapshot().alerts, 1);
    }

    #[test]
    fn misses_spread_over_more_updates_than_the_window_do_not_alert() {
        let (guard, alerts) = alerting(2, Window::Updates(3));
        let observe = |verdict| guard.observe(Kind::Account, verdict, &update(&[0]));
        for _ in 0..4 {
            observe(Verdict::Failed);
            observe(Verdict::Decoded);
            observe(Verdict::Decoded);
        }
        assert!(alerts.lock().unwrap().is_empty());
        // A new window opens at the next miss, and its second miss alerts.
        observe(Verdict::Failed);
        observe(Verdict::Failed);
        assert_eq!(alerts.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_time_window_closes_after_its_span() {
        let (guard, alerts) = alerting(2, Window::Time(Duration::from_secs(10)));
        let start = Instant::now();
        let at = |secs, data: &[u8]| {
            guard.observe_at(
                Kind::Instruction,
                Verdict::Failed,
                &update(data),
                Some(start + Duration::from_secs(secs)),
            )
        };
        at(0, &[1]);
        at(11, &[2]);
        assert!(
            alerts.lock().unwrap().is_empty(),
            "the window closed at 10s"
        );
        at(15, &[3]);
        at(16, &[4]);
        let got = alerts.lock().unwrap().clone();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].first.head, [2]);
        // The window stays open, and fired, until 21s.
        at(20, &[5]);
        at(22, &[6]);
        at(23, &[7]);
        assert_eq!(alerts.lock().unwrap().len(), 2);
    }

    #[test]
    fn prometheus_text_has_one_series_per_kind_and_outcome() {
        let mut named = Guard::new(PROGRAM, "a\"b\\c\nd");
        named = named.alert_after(1, Window::Updates(10), |_| {});
        named.observe(Kind::Account, Verdict::Failed, &update(&[0]));
        named.observe(Kind::Account, Verdict::Decoded, &update(&[0]));
        let other = Guard::new([0; 32], "q");
        let text = prometheus([&named, &other]);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "# HELP idl_drift_guard_updates_total Updates that a guarded parser or decoder saw, by kind and outcome.");
        assert_eq!(lines[1], "# TYPE idl_drift_guard_updates_total counter");
        assert_eq!(
            lines
                .iter()
                .filter(|l| l.starts_with("idl_drift_guard_updates_total{"))
                .count(),
            2 * 3 * 7
        );
        let program = bs58::encode(PROGRAM).into_string();
        let labels = format!(r#"program="{program}",name="a\"b\\c\nd""#);
        assert!(text.contains(&format!(
            "idl_drift_guard_updates_total{{{labels},kind=\"account\",outcome=\"decode_error\"}} 1\n"
        )));
        assert!(text.contains(&format!(
            "idl_drift_guard_updates_total{{{labels},kind=\"account\",outcome=\"decoded\"}} 1\n"
        )));
        assert!(text.contains(&format!("idl_drift_guard_alerts_total{{{labels}}} 1\n")));
        assert!(text.contains("# TYPE idl_drift_guard_alerts_total counter\n"));
        assert!(text.ends_with("name=\"q\"} 0\n"));
    }

    #[test]
    fn a_miss_prints_its_place() {
        let miss = Miss {
            kind: Kind::Account,
            outcome: Outcome::ExtraBytes,
            type_name: Some("Pool".into()),
            len: 166,
            expected_len: Some(115),
            head: vec![0xab, 1],
            slot: Some(9),
            key: Some(vec![0; 32]),
        };
        assert_eq!(
            miss.to_string(),
            "account extra_bytes (Pool): 166 bytes, the IDL fixes 115, starts with 0xab01, slot 9, 11111111111111111111111111111111"
        );
    }

    /// One account type and one event, at `address`.
    fn idl(address: &str) -> String {
        format!(
            r#"{{"address": "{address}",
            "metadata": {{"name": "t", "version": "0.1.0", "spec": "0.1.0"}},
            "accounts": [{{"name": "Pool", "discriminator": [1, 1, 1, 1, 1, 1, 1, 1]}}],
            "events": [{{"name": "Ev", "discriminator": [2, 2, 2, 2, 2, 2, 2, 2]}}],
            "types": [
                {{"name": "Pool", "type": {{"kind": "struct", "fields": [{{"name": "a", "type": "u64"}}]}}}},
                {{"name": "Ev", "type": {{"kind": "struct", "fields": [{{"name": "x", "type": "u8"}}]}}}}
            ]}}"#
        )
    }

    #[test]
    fn a_foreign_update_is_filtered_before_any_lookup() {
        let guard = Guard::from_idl(&idl(&bs58::encode(PROGRAM).into_string())).unwrap();
        // Another program's self-CPI event: not an event of this program.
        let data = [
            &[0xe4, 0x45, 0xa5, 0x2e, 0x51, 0xcb, 0x9a, 0x1d][..],
            &[2; 8],
            &[0],
        ]
        .concat();
        let foreign = Update {
            program: Some(&[8; 32]),
            ..update(&data)
        };
        guard.observe(Kind::Instruction, Verdict::Failed, &foreign);
        let snapshot = guard.snapshot();
        assert_eq!(snapshot.instructions.filtered, 1);
        assert_eq!(snapshot.events.filtered, 0);
    }

    #[test]
    fn a_miss_in_a_fired_window_does_not_wait_for_the_lock() {
        let (guard, alerts) = alerting(1, Window::Updates(100));
        guard.observe(Kind::Account, Verdict::Failed, &update(&[1]));
        assert_eq!(alerts.lock().unwrap().len(), 1);
        let guard = &guard;
        let held = guard.window.lock().unwrap();
        let (sent, done) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(move || {
                guard.observe(Kind::Account, Verdict::Failed, &update(&[2]));
                sent.send(()).unwrap();
            });
            let finished = done.recv_timeout(Duration::from_secs(2));
            drop(held);
            assert!(finished.is_ok(), "a miss in a fired window took the lock");
        });
        assert_eq!(guard.snapshot().accounts.decode_error, 2);
    }

    #[test]
    fn a_program_id_replaces_a_missing_idl_address() {
        assert!(Guard::from_idl(&idl("")).is_err());
        let guard = Guard::from_idl_for(PROGRAM, &idl("")).unwrap();
        assert_eq!(guard.program(), &PROGRAM);
        assert_eq!(guard.shapes(Kind::Account).count(), 1);
        assert_eq!(guard.program_mismatch(), None);
        assert!(guard.check_program(&PROGRAM));
        // A decoder that returns a value for another program: the program id is wrong.
        for other in [[8; 32], [9; 32]] {
            let foreign = Update {
                program: Some(&other),
                ..update(&[0])
            };
            assert_eq!(
                guard.observe(Kind::Account, Verdict::Decoded, &foreign),
                Outcome::Filtered
            );
        }
        assert_eq!(guard.program_mismatch(), Some(&[8; 32]));
        assert!(!guard.check_program(&[9; 32]));
        assert_eq!(
            guard.program_mismatch(),
            Some(&[8; 32]),
            "the first one stays"
        );
    }

    #[test]
    fn prometheus_sums_guards_with_the_same_labels() {
        let a = Guard::new(PROGRAM, "p");
        let b = Guard::new(PROGRAM, "p");
        a.observe(Kind::Account, Verdict::Decoded, &update(&[0]));
        b.observe(Kind::Account, Verdict::Decoded, &update(&[0]));
        let text = prometheus([&a, &b, &a]);
        let series = r#"name="p",kind="account",outcome="decoded"}"#;
        assert_eq!(text.matches(series).count(), 1, "{text}");
        // The same guard twice counts once.
        assert!(text.contains(&format!("{series} 2\n")), "{text}");
        assert_eq!(text.matches("idl_drift_guard_alerts_total{").count(), 1);
    }
}
