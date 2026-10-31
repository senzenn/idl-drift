//! Event rules: the content discriminator and the self-CPI envelope around the payload.

mod common;

use common::{messages, parse};
use idl_drift::diff;

#[test]
fn event_envelope_length_8_to_1_is_breaking() {
    // Anchor wraps self-CPI events in an 8-byte tag, Pinocchio often in 1 byte; switching
    // moves where the payload starts.
    let event = |envelope: &str, offset: u8| {
        parse(&format!(
            r#"{{"address":"A","metadata":{{"name":"e","version":"1","spec":"0.1.0"}},
  "instructions":[{{"name":"noop","discriminator":[1],"accounts":[],"args":[]}}],
  "events":[{{"name":"Trade","discriminator":[1,2,3,4,5,6,7,8],
    "envelope_discriminator":[{envelope}],"envelope_payload_offset":{offset}}}],
  "types":[{{"name":"Trade","type":{{"kind":"struct","fields":[]}}}}]}}"#
        ))
    };
    assert_eq!(
        messages(&event("228,69,165,46,81,203,154,29", 8), &event("254", 1)),
        [
            "event envelope discriminator LENGTH changed (Trade): 8->1 bytes (scheme swap, e.g. Anchor 8-byte <-> Pinocchio 1-byte)",
            "event envelope payload offset changed (Trade): 8->1 (self-CPI parse starts elsewhere)"
        ]
    );
}

#[test]
fn codama_events_are_top_level_with_discriminator() {
    let idl = parse(include_str!("fixtures/codama_tiny.json"));
    assert_eq!(idl.events.len(), 1);
    assert_eq!(idl.events[0].name, "pong");
    assert_eq!(
        idl.events[0].discriminator,
        vec![0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x00, 0x11]
    );
    let r = diff(&idl, &idl);
    assert!(r.changes.is_empty(), "got {:?}", r.changes);
}

#[test]
fn codama_event_envelope_comes_from_the_discriminator_chain() {
    // [tag @0, event discriminator @8] is Shipstern's envelope rule; one discriminator is none.
    let event = |discriminators: &str| {
        format!(
            r#"{{"kind":"rootNode","standard":"codama","version":"1.5.1","program":{{
            "kind":"programNode","name":"e","publicKey":"11111111111111111111111111111111",
            "version":"0.1.0","accounts":[],"instructions":[],"definedTypes":[],"pdas":[],
            "errors":[],"events":[{{"kind":"eventNode","name":"trade","discriminators":[{discriminators}],
            "data":{{"kind":"structTypeNode","fields":[]}}}}]}}}}"#
        )
    };
    let constant = |hex: &str, offset: u8| {
        format!(
            r#"{{"kind":"constantDiscriminatorNode","offset":{offset},"constant":{{"kind":"constantValueNode",
            "type":{{"kind":"fixedSizeTypeNode","size":8,"type":{{"kind":"bytesTypeNode"}}}},
            "value":{{"kind":"bytesValueNode","data":"{hex}","encoding":"base16"}}}}}}"#
        )
    };
    // Declared out of order on purpose: Shipstern sorts the chain by offset.
    let chained = parse(&event(&format!(
        "{},{}",
        constant("0102030405060708", 8),
        constant("e445a52e51cb9a1d", 0)
    )));
    let e = &chained.events[0];
    assert_eq!(e.discriminator, [1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(
        e.envelope_discriminator,
        [0xe4, 0x45, 0xa5, 0x2e, 0x51, 0xcb, 0x9a, 0x1d]
    );
    assert_eq!(e.envelope_payload_offset, Some(8));
    let single = parse(&event(&constant("0102030405060708", 0)));
    let e = &single.events[0];
    assert_eq!(e.discriminator, [1, 2, 3, 4, 5, 6, 7, 8]);
    assert!(e.envelope_discriminator.is_empty());
}
