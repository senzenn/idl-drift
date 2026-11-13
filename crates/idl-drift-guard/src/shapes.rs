//! What the IDL fixes for each type: its discriminators and its length.

use std::cmp::Reverse;

use idl_drift::layout::TypeIndex;
use idl_drift::model::{Discriminator, Event, Format, Idl, Instruction, NamedDisc, Type};

use crate::Kind;

/// Anchor's `EVENT_IX_TAG`: the envelope that `emit_cpi!` puts before an event.
const ANCHOR_EVENT_TAG: [u8; 8] = [0xe4, 0x45, 0xa5, 0x2e, 0x51, 0xcb, 0x9a, 0x1d];

#[derive(Debug, Clone)]
pub(crate) struct Shape {
    pub(crate) name: String,
    /// Each byte discriminator with its offset. All of them must match.
    discs: Vec<(usize, Vec<u8>)>,
    /// The data length that selects the type: a size discriminator, or the size of a type
    /// with no byte discriminator.
    select_len: Option<usize>,
    pub(crate) payload_start: usize,
    /// The full length that the IDL fixes; `None` when it varies.
    pub(crate) len: Option<usize>,
    /// `len` is a minimum: longer data is not extra bytes.
    pub(crate) at_least: bool,
}

impl Shape {
    fn discs_match(&self, data: &[u8]) -> bool {
        self.discs
            .iter()
            .all(|(at, disc)| data.get(*at..).is_some_and(|rest| rest.starts_with(disc)))
    }

    fn disc_bytes(&self) -> usize {
        self.discs.iter().map(|(_, disc)| disc.len()).sum()
    }

    fn disc_end(&self) -> usize {
        self.discs
            .iter()
            .map(|(at, disc)| at.saturating_add(disc.len()))
            .max()
            .unwrap_or(0)
    }
}

pub(crate) enum Lookup<'a> {
    /// The IDL has no entry of this kind, so only the decoder's verdict counts.
    NoIdl,
    Type(&'a Shape),
    /// The discriminators match, but the length selects no type.
    WrongLen,
    /// The data ends before the shortest discriminator.
    Short,
    Unknown,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Table {
    /// Most discriminator bytes first, so a longer discriminator wins over its prefix.
    shapes: Vec<Shape>,
    min_disc_end: usize,
}

impl Table {
    fn new(mut shapes: Vec<Shape>) -> Self {
        // A type with neither a discriminator nor a length cannot be selected.
        shapes.retain(|s| !s.discs.is_empty() || s.select_len.is_some());
        shapes.sort_by_key(|s| Reverse(s.disc_bytes()));
        let min_disc_end = shapes
            .iter()
            .filter(|s| !s.discs.is_empty())
            .map(Shape::disc_end)
            .min()
            .unwrap_or(0);
        Self {
            shapes,
            min_disc_end,
        }
    }

    pub(crate) fn lookup(&self, data: &[u8]) -> Lookup<'_> {
        if self.shapes.is_empty() {
            return Lookup::NoIdl;
        }
        let mut hit = false;
        for shape in &self.shapes {
            if shape.discs_match(data) {
                if shape.select_len.is_none_or(|len| len == data.len()) {
                    return Lookup::Type(shape);
                }
                hit = true;
            }
        }
        if hit {
            Lookup::WrongLen
        } else if data.len() < self.min_disc_end {
            Lookup::Short
        } else {
            Lookup::Unknown
        }
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &Shape> {
        self.shapes.iter()
    }

    /// Set the length of each type named `name`. `false` when there is none.
    pub(crate) fn expect_len(&mut self, name: &str, len: usize, at_least: bool) -> bool {
        let mut found = false;
        for shape in self.shapes.iter_mut().filter(|s| s.name == name) {
            shape.len = Some(len);
            shape.at_least = at_least;
            // A type with no discriminator is selected by its exact length.
            if shape.discs.is_empty() && !at_least {
                shape.select_len = Some(len);
            }
            found = true;
        }
        found
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Shapes {
    tables: [Table; 3],
    /// Self-CPI envelopes, longest first: instruction data that starts with one is an event.
    envelopes: Vec<Vec<u8>>,
}

impl Shapes {
    pub(crate) fn from_idl(idl: &Idl) -> Self {
        let index = TypeIndex::new(idl);
        let accounts = idl
            .accounts
            .iter()
            .map(|nd| account(idl, &index, nd))
            .collect();
        let instructions = idl
            .instructions
            .iter()
            .map(|ix| instruction(&index, ix))
            .collect();
        let events = idl.events.iter().map(|ev| event(&index, ev)).collect();
        let mut envelopes: Vec<Vec<u8>> = idl
            .events
            .iter()
            .map(|ev| envelope(ev).0.to_vec())
            .filter(|prefix| !prefix.is_empty())
            .collect();
        envelopes.sort_by_key(|prefix| Reverse(prefix.len()));
        envelopes.dedup();
        Self {
            tables: [
                Table::new(accounts),
                Table::new(instructions),
                Table::new(events),
            ],
            envelopes,
        }
    }

    pub(crate) fn table(&self, kind: Kind) -> &Table {
        &self.tables[kind.index()]
    }

    pub(crate) fn table_mut(&mut self, kind: Kind) -> &mut Table {
        &mut self.tables[kind.index()]
    }

    pub(crate) fn is_event(&self, data: &[u8]) -> bool {
        self.envelopes.iter().any(|prefix| data.starts_with(prefix))
    }
}

fn defined(name: &str) -> Type {
    Type::Defined {
        name: name.to_string(),
        generics: Vec::new(),
    }
}

fn discs(
    offset: usize,
    first: &[u8],
    extra: &[(usize, Discriminator)],
    shift: usize,
) -> Vec<(usize, Vec<u8>)> {
    std::iter::once((offset, first))
        .chain(extra.iter().map(|(at, disc)| (*at, disc.as_slice())))
        .filter(|(_, disc)| !disc.is_empty())
        .filter_map(|(at, disc)| Some((at.checked_add(shift)?, disc.to_vec())))
        .collect()
}

fn account(idl: &Idl, index: &TypeIndex<'_>, nd: &NamedDisc) -> Shape {
    let payload_start = nd.payload_start();
    // The computed size first: a Codama `size` can leave out zero-copy padding.
    let computed = index.get(&nd.name).and_then(|td| {
        let body = if td.is_zero_copy() {
            index.struct_layout(td).ok().map(|layout| layout.size)
        } else {
            index.borsh_size(&defined(&nd.name)).ok().flatten()
        };
        body?.checked_add(payload_start)
    });
    // The same order as for an instruction: a size discriminator first.
    let len = nd.size_discriminator.or(computed).or(nd.size);
    let discs = discs(
        nd.discriminator_offset,
        &nd.discriminator,
        &nd.extra_discriminators,
        0,
    );
    // Shank programs tell account types apart by a field, not by size.
    let by_size = discs.is_empty() && idl.format != Format::Shank;
    Shape {
        name: nd.name.clone(),
        discs,
        select_len: nd
            .size_discriminator
            .or(if by_size { nd.size.or(computed) } else { None }),
        payload_start,
        len,
        at_least: false,
    }
}

fn instruction(index: &TypeIndex<'_>, ix: &Instruction) -> Shape {
    let payload_start = ix.payload_start();
    let args = ix.args.iter().try_fold(0usize, |sum, field| {
        sum.checked_add(index.borsh_size(&field.ty).ok().flatten()?)
    });
    Shape {
        name: ix.name.clone(),
        discs: discs(
            ix.discriminator_offset,
            &ix.discriminator,
            &ix.extra_discriminators,
            0,
        ),
        select_len: ix.size_discriminator,
        payload_start,
        len: ix
            .size_discriminator
            .or_else(|| args?.checked_add(payload_start)),
        at_least: false,
    }
}

/// The IDL's envelope for `event`, else Anchor's: an Anchor IDL cannot describe one.
fn envelope(ev: &Event) -> (&[u8], usize) {
    match ev.effective_payload_offset() {
        Some(len) => (&ev.envelope_discriminator, len),
        None => (&ANCHOR_EVENT_TAG, ANCHOR_EVENT_TAG.len()),
    }
}

/// An event as the instruction data of its self-CPI: the envelope, then the event bytes.
fn event(index: &TypeIndex<'_>, ev: &Event) -> Shape {
    let (prefix, envelope_len) = envelope(ev);
    let mut all = Vec::new();
    if !prefix.is_empty() {
        all.push((0, prefix.to_vec()));
    }
    all.extend(discs(
        ev.discriminator_offset,
        &ev.discriminator,
        &ev.extra_discriminators,
        envelope_len,
    ));
    let payload_start = envelope_len.saturating_add(ev.payload_start());
    let len = index
        .borsh_size(&defined(&ev.name))
        .ok()
        .flatten()
        .and_then(|body| body.checked_add(payload_start));
    Shape {
        name: ev.name.clone(),
        discs: all,
        select_len: None,
        payload_start,
        len,
        at_least: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_size_discriminator_sets_the_length_of_an_account_as_of_an_instruction() {
        let json = r#"{"address": "11111111111111111111111111111111",
            "metadata": {"name": "t", "version": "0.1.0", "spec": "0.1.0"},
            "accounts": [{"name": "Pool", "discriminator": [1, 1, 1, 1, 1, 1, 1, 1]}],
            "types": [{"name": "Pool", "type": {"kind": "struct", "fields": [{"name": "a", "type": "u64"}]}}]}"#;
        let mut idl = Idl::from_json(json).unwrap();
        assert_eq!(
            Shapes::from_idl(&idl)
                .table(Kind::Account)
                .iter()
                .next()
                .unwrap()
                .len,
            Some(16)
        );
        idl.accounts[0].size_discriminator = Some(20);
        let shapes = Shapes::from_idl(&idl);
        assert_eq!(
            shapes.table(Kind::Account).iter().next().unwrap().len,
            Some(20)
        );
    }
}
