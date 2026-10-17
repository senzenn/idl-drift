//! Match samples to IDL entries: by discriminator, by account size, and by self-CPI envelope.

use std::fmt;

use idl_drift::model::{
    Account, AccountItem, Discriminator, Event, Format, Idl, Instruction, NamedDisc, Type,
};

use crate::layout::{Reading, TypeIndex};
use crate::samples::{EventSample, EventSource, InstructionSample};
use crate::text::{bytes, prefix_hex};

/// Anchor's `EVENT_IX_TAG` in little-endian: the envelope `emit_cpi!` puts before an event.
pub(crate) const EVENT_IX_TAG: [u8; 8] = [0xe4, 0x45, 0xa5, 0x2e, 0x51, 0xcb, 0x9a, 0x1d];

/// A discriminator: where it starts in the data, and its bytes.
pub(crate) type Disc<'a> = (usize, &'a [u8]);

/// Every discriminator of one entry, the first one first. Codama requires all to match.
pub(crate) type Discs<'a> = Vec<Disc<'a>>;

/// `(discriminators, index)` pairs, most discriminator bytes first. An entry whose first
/// discriminator is empty is dropped.
pub(crate) type DiscTable<'a> = Vec<(Discs<'a>, usize)>;

/// An IDL entry that a sample is matched to by its discriminators.
pub(crate) trait Discriminated {
    fn discs(&self) -> Discs<'_>;
}

fn discs<'a>(offset: usize, first: &'a [u8], extra: &'a [(usize, Discriminator)]) -> Discs<'a> {
    std::iter::once((offset, first))
        .chain(extra.iter().map(|(at, d)| (*at, d.as_slice())))
        .collect()
}

impl Discriminated for NamedDisc {
    fn discs(&self) -> Discs<'_> {
        discs(
            self.discriminator_offset,
            &self.discriminator,
            &self.extra_discriminators,
        )
    }
}

impl Discriminated for Instruction {
    fn discs(&self) -> Discs<'_> {
        discs(
            self.discriminator_offset,
            &self.discriminator,
            &self.extra_discriminators,
        )
    }
}

/// Event discriminator offsets count from the start of the event bytes, after any envelope
/// is removed.
impl Discriminated for Event {
    fn discs(&self) -> Discs<'_> {
        discs(
            self.discriminator_offset,
            &self.discriminator,
            &self.extra_discriminators,
        )
    }
}

fn disc_table<'a>(entries: impl Iterator<Item = Discs<'a>>) -> DiscTable<'a> {
    let mut table: DiscTable<'a> = entries
        .enumerate()
        .filter(|(_, d)| d.first().is_some_and(|(_, first)| !first.is_empty()))
        .map(|(i, d)| (d, i))
        .collect();
    table.sort_by_key(|(d, i)| {
        let len: usize = d.iter().map(|(_, bytes)| bytes.len()).sum();
        (std::cmp::Reverse(len), *i)
    });
    table
}

pub(crate) fn account_table(idl: &Idl) -> DiscTable<'_> {
    disc_table(idl.accounts.iter().map(Discriminated::discs))
}

pub(crate) fn instruction_table(idl: &Idl) -> DiscTable<'_> {
    disc_table(idl.instructions.iter().map(Discriminated::discs))
}

pub(crate) fn event_table(idl: &Idl) -> DiscTable<'_> {
    disc_table(idl.events.iter().map(Discriminated::discs))
}

/// Every entry whose discriminators all match `data` and hold the most bytes.
pub(crate) fn longest_match(table: &[(Discs<'_>, usize)], data: &[u8]) -> Option<Vec<usize>> {
    let at = |(offset, disc): &Disc<'_>| data.get(*offset..).is_some_and(|d| d.starts_with(disc));
    let (found, _) = table.iter().find(|(d, _)| d.iter().all(at))?;
    Some(
        table
            .iter()
            .filter(|(d, _)| d == found)
            .map(|(_, i)| *i)
            .collect(),
    )
}

/// Account types with no discriminator and a known size: the IDL's declared `size`, else
/// the type's fixed size. Codama's size discriminator tells such accounts apart by length.
fn sized_account_types(idl: &Idl) -> Vec<(usize, usize)> {
    // Shank programs tell account types apart by a field, not by size.
    if idl.format == Format::Shank {
        return Vec::new();
    }
    let index = TypeIndex::new(idl);
    idl.accounts
        .iter()
        .enumerate()
        .filter(|(_, nd)| nd.discriminator.is_empty())
        .filter_map(|(i, nd)| {
            let computed = || {
                let td = index.get(&nd.name)?;
                let reading = if td.is_zero_copy() {
                    Reading::ZeroCopy
                } else {
                    Reading::Borsh
                };
                let ty = Type::Defined {
                    name: nd.name.clone(),
                    generics: Vec::new(),
                };
                index.reading_size(&ty, reading).ok()
            };
            nd.size.or_else(computed).map(|size| (i, size))
        })
        .collect()
}

/// Like [`longest_match`] for instructions, but a Codama size discriminator must match too.
pub(crate) fn instruction_match(
    idl: &Idl,
    table: &[(Discs<'_>, usize)],
    data: &[u8],
) -> Option<Vec<usize>> {
    let sized: DiscTable<'_> = table
        .iter()
        .filter(|(_, i)| {
            idl.instructions[*i]
                .size_discriminator
                .is_none_or(|size| size == data.len())
        })
        .cloned()
        .collect();
    longest_match(&sized, data)
}

/// Among instructions sharing a discriminator, prefer an exact account count, then the
/// largest count the sample satisfies (Shipstern's dispatch order).
fn pick_by_account_count(
    instructions: &[Instruction],
    candidates: &[usize],
    passed: usize,
) -> usize {
    let count = |i: &usize| flatten(&instructions[*i].accounts).len();
    candidates
        .iter()
        .find(|i| count(i) == passed)
        .or_else(|| {
            candidates
                .iter()
                .filter(|i| count(i) <= passed)
                .max_by_key(|i| count(i))
        })
        .or_else(|| candidates.first())
        .copied()
        .unwrap_or_default()
}

/// The instruction's accounts in the order they are passed, composite groups expanded.
pub(crate) fn flatten(items: &[AccountItem]) -> Vec<&Account> {
    let mut out = Vec::new();
    for item in items {
        match item {
            AccountItem::Single(a) => out.push(a),
            AccountItem::Composite { accounts, .. } => out.extend(flatten(accounts)),
        }
    }
    out
}

/// The self-CPI envelope before an event: the bytes it starts with, and its length.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Envelope<'a> {
    prefix: &'a [u8],
    len: usize,
}

pub(crate) const ANCHOR_ENVELOPE: Envelope<'static> = Envelope {
    prefix: &EVENT_IX_TAG,
    len: EVENT_IX_TAG.len(),
};

impl Envelope<'_> {
    pub(crate) fn wraps(&self, data: &[u8]) -> bool {
        data.starts_with(self.prefix)
    }
}

impl fmt::Display for Envelope<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.prefix.is_empty() {
            write!(f, "of {}", bytes(self.len))
        } else {
            f.write_str(&prefix_hex(self.prefix))
        }
    }
}

/// The IDL's envelope for `event`, else Anchor's: an Anchor IDL cannot describe one.
fn envelope(event: &Event) -> Envelope<'_> {
    match event.effective_payload_offset() {
        Some(len) => Envelope {
            prefix: &event.envelope_discriminator,
            len,
        },
        None => ANCHOR_ENVELOPE,
    }
}

/// The IDL's envelopes, longest prefix first, so an envelope wins over a shorter one it extends.
pub(crate) fn envelopes(idl: &Idl) -> Vec<Envelope<'_>> {
    let mut envelopes: Vec<Envelope<'_>> = idl.events.iter().map(envelope).collect();
    envelopes.sort_by(|a, b| b.prefix.len().cmp(&a.prefix.len()).then(a.cmp(b)));
    envelopes.dedup();
    envelopes
}

/// The envelopes that mark a self-CPI event instruction. Shipstern uses Anchor's tag when
/// the IDL declares none.
pub(crate) fn cpi_envelopes(idl: &Idl) -> Vec<Envelope<'_>> {
    let mut cpi: Vec<Envelope<'_>> = envelopes(idl)
        .into_iter()
        .filter(|e| !e.prefix.is_empty())
        .collect();
    if cpi.is_empty() {
        cpi.push(ANCHOR_ENVELOPE);
    }
    cpi
}

/// The event bytes, `[discriminator][payload]`, with a self-CPI sample's envelope removed;
/// `None` for a cpi sample that does not start with any envelope the IDL expects.
pub(crate) fn event_body<'s>(
    sample: &'s EventSample,
    envelopes: &[Envelope<'_>],
) -> Option<&'s [u8]> {
    match sample.source {
        EventSource::Log => Some(&sample.data),
        EventSource::Cpi => envelopes
            .iter()
            .filter(|e| sample.data.starts_with(e.prefix))
            .find_map(|e| sample.data.get(e.len..)),
    }
}

/// Picks the IDL entry for a sample: the longest discriminator, then the account size for
/// accounts without one, then the account count for instructions that share one.
pub(crate) struct Classifier<'a> {
    pub(crate) idl: &'a Idl,
    pub(crate) accounts: DiscTable<'a>,
    /// `(account index, size)` for account types without a discriminator.
    pub(crate) sized: Vec<(usize, usize)>,
    pub(crate) instructions: DiscTable<'a>,
    pub(crate) events: DiscTable<'a>,
    pub(crate) envelopes: Vec<Envelope<'a>>,
}

impl<'a> Classifier<'a> {
    pub(crate) fn new(idl: &'a Idl) -> Self {
        Self {
            idl,
            accounts: account_table(idl),
            sized: sized_account_types(idl),
            instructions: instruction_table(idl),
            events: event_table(idl),
            envelopes: envelopes(idl),
        }
    }

    pub(crate) fn account(&self, data: &[u8]) -> Option<usize> {
        if let Some(found) = longest_match(&self.accounts, data) {
            return found.first().copied();
        }
        match self.fits(data.len()).as_slice() {
            [t] => Some(*t),
            _ => None,
        }
    }

    /// The sized account types that `len` bytes fit.
    pub(crate) fn fits(&self, len: usize) -> Vec<usize> {
        self.sized
            .iter()
            .filter(|(_, size)| *size == len)
            .map(|(t, _)| *t)
            .collect()
    }

    pub(crate) fn instruction(&self, sample: &InstructionSample) -> Option<usize> {
        let candidates = instruction_match(self.idl, &self.instructions, &sample.data)?;
        Some(pick_by_account_count(
            &self.idl.instructions,
            &candidates,
            sample.accounts.len(),
        ))
    }

    pub(crate) fn event_body<'s>(&self, sample: &'s EventSample) -> Option<&'s [u8]> {
        event_body(sample, &self.envelopes)
    }

    /// The event for `body`, the event bytes with any envelope removed.
    pub(crate) fn event(&self, body: &[u8]) -> Option<usize> {
        longest_match(&self.events, body)?.first().copied()
    }
}
