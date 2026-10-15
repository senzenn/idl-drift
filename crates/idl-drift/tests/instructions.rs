//! Instruction rules: the instruction set, discriminators, accounts by position, args,
//! shared discriminators, and the names Shipstern generates.

mod common;

use common::{empty_idl, idl_kinds, kinds, max_sev, messages, parse, AMM_OLD};
use idl_drift::model::{
    Account, AccountItem, Format, Idl, Instruction, NamedDisc, OptionalAccounts,
};
use idl_drift::{diff, ChangeKind, Severity};

fn bare_idl(instructions: Vec<Instruction>) -> Idl {
    Idl {
        instructions,
        ..empty_idl()
    }
}

fn ix(name: &str, disc: Vec<u8>, accounts: Vec<AccountItem>) -> Instruction {
    Instruction {
        name: name.into(),
        discriminator: disc,
        discriminator_offset: 0,
        extra_discriminators: vec![],
        size_discriminator: None,
        accounts,
        args: vec![],
        optional_accounts: OptionalAccounts::ProgramId,
    }
}

fn account(name: &str, optional: bool) -> AccountItem {
    AccountItem::Single(Account {
        name: name.into(),
        writable: false,
        signer: false,
        may_sign: false,
        optional,
        address: None,
        address_is_default: false,
        pda: None,
    })
}

fn acct(name: &str) -> AccountItem {
    account(name, false)
}

fn opt_acct(name: &str) -> AccountItem {
    account(name, true)
}

#[test]
fn missing_discriminator_deserializes_and_is_stable() {
    // Native programs may give no discriminator.
    let native = r#"{"address":"A","metadata":{"name":"n","version":"1","spec":"0.1.0"},
      "instructions":[{"name":"transfer","accounts":[],"args":[{"name":"amount","type":"u64"}]}]}"#;
    assert_eq!(max_sev(native, native), None);
}

#[test]
fn duplicate_instruction_name_is_flagged() {
    let dup = r#"{"address":"A","metadata":{"name":"d","version":"1","spec":"0.1.0"},
      "instructions":[{"name":"swap","discriminator":[1],"accounts":[],"args":[]},
                      {"name":"swap","discriminator":[2],"accounts":[],"args":[]}]}"#;
    assert_eq!(
        kinds(dup, dup),
        [(Severity::Breaking, ChangeKind::InstructionDuplicate)]
    );
}

#[test]
fn new_instruction_reusing_a_discriminator_is_breaking() {
    let old = r#"{"address":"A","metadata":{"name":"t","version":"0","spec":"0.1.0"},
      "instructions":[{"name":"a","discriminator":[1],"accounts":[{"name":"x"}],"args":[]}]}"#;
    let new = r#"{"address":"A","metadata":{"name":"t","version":"0","spec":"0.1.0"},
      "instructions":[{"name":"a","discriminator":[1],"accounts":[{"name":"x"}],"args":[]},
        {"name":"b","discriminator":[1],"accounts":[{"name":"x"},{"name":"y"}],"args":[{"name":"n","type":"u64"}]}]}"#;
    assert_eq!(
        messages(&parse(old), &parse(new)),
        ["new instruction b reuses discriminator 01 of [a]: parsers built from the old IDL route it there and misread its args"]
    );
}

#[test]
fn same_disc_same_account_count_is_breaking() {
    let disc = vec![0x01, 0x02, 0x03, 0x04];
    let old = bare_idl(vec![ix("swap_a", disc.clone(), vec![acct("a"), acct("b")])]);
    let new = bare_idl(vec![
        ix("swap_a", disc.clone(), vec![acct("a"), acct("b")]),
        ix("swap_b", disc, vec![acct("c"), acct("d")]),
    ]);
    assert_eq!(
        idl_kinds(&old, &new),
        [
            (Severity::Breaking, ChangeKind::DiscriminatorReused),
            (Severity::Breaking, ChangeKind::Collision)
        ]
    );
}

#[test]
fn same_disc_different_account_count_is_not_a_change() {
    // Shipstern tells the two apart by account count.
    let disc = vec![0x01, 0x02, 0x03, 0x04];
    let idl = bare_idl(vec![
        ix("swap_a", disc.clone(), vec![acct("a"), acct("b")]),
        ix("swap_b", disc, vec![acct("a"), acct("b"), acct("c")]),
    ]);
    assert_eq!(idl_kinds(&idl, &idl), []);
}

#[test]
fn disambiguation_order_change_is_dangerous() {
    // Counts {2,4} become {2,3} by dropping an optional tail account, which alone is only
    // Dangerous.
    let disc = vec![0xaa, 0xbb];
    let old = bare_idl(vec![
        ix("swap_a", disc.clone(), vec![acct("a"), acct("b")]),
        ix(
            "swap_b",
            disc.clone(),
            vec![acct("a"), acct("b"), acct("c"), opt_acct("d")],
        ),
    ]);
    let new = bare_idl(vec![
        ix("swap_a", disc.clone(), vec![acct("a"), acct("b")]),
        ix("swap_b", disc, vec![acct("a"), acct("b"), acct("c")]),
    ]);
    assert_eq!(
        idl_kinds(&old, &new),
        [
            (Severity::Dangerous, ChangeKind::OptionalAccountRemoved),
            (Severity::Dangerous, ChangeKind::DispatchOrderChanged)
        ]
    );
}

#[test]
fn removing_several_optional_tail_accounts_is_dangerous() {
    let drop_tail = |accounts: Vec<AccountItem>| {
        diff(
            &bare_idl(vec![ix("f", vec![1], accounts)]),
            &bare_idl(vec![ix("f", vec![1], vec![acct("a")])]),
        )
    };
    let report = drop_tail(vec![acct("a"), opt_acct("b"), opt_acct("c")]);
    let max = report.changes.iter().map(|c| c.severity).max();
    assert_eq!(max, Some(Severity::Dangerous), "{:?}", report.changes);
    // A required account in the removed block still breaks.
    assert!(drop_tail(vec![acct("a"), acct("b"), opt_acct("c")]).has_breaking());
}

#[test]
fn account_flag_flip_at_same_position_is_dangerous() {
    let new = AMM_OLD.replace(
        r#"{"name":"user","signer":true}"#,
        r#"{"name":"user","signer":true,"writable":true}"#,
    );
    assert_eq!(
        kinds(AMM_OLD, &new),
        [(Severity::Dangerous, ChangeKind::AccountFlagsChanged)]
    );
}

#[test]
fn account_rename_at_same_position_is_dangerous_but_a_swap_is_breaking() {
    let renamed = AMM_OLD.replace(
        r#"{"name":"user","signer":true}"#,
        r#"{"name":"payer","signer":true}"#,
    );
    assert_eq!(
        kinds(AMM_OLD, &renamed),
        [(Severity::Dangerous, ChangeKind::AccountRenamed)]
    );
    let swapped = AMM_OLD.replace(
        r#"[{"name":"pool","writable":true},{"name":"user","signer":true}]"#,
        r#"[{"name":"user","signer":true},{"name":"pool","writable":true}]"#,
    );
    assert!(kinds(AMM_OLD, &swapped)
        .iter()
        .all(|k| *k == (Severity::Breaking, ChangeKind::AccountMoved)));
}

#[test]
fn arg_rename_is_dangerous_and_arg_type_change_is_breaking() {
    let renamed = AMM_OLD.replace(r#""name":"amountIn""#, r#""name":"amount""#);
    assert_eq!(
        kinds(AMM_OLD, &renamed),
        [(Severity::Dangerous, ChangeKind::ArgRenamed)]
    );
    let widened = AMM_OLD.replace(
        r#"{"name":"amountIn","type":"u64"}"#,
        r#"{"name":"amountIn","type":"u128"}"#,
    );
    assert_eq!(
        kinds(AMM_OLD, &widened),
        [(Severity::Breaking, ChangeKind::ArgChanged)]
    );
}

#[test]
fn instruction_names_equal_up_to_ascii_case_are_breaking() {
    // Shipstern turns both into `parse_abc` and the generated parser fails to compile.
    let idl = |a: &str, b: &str| {
        format!(
            r#"{{"address":"A","metadata":{{"name":"c","version":"1","spec":"0.1.0"}},
              "instructions":[{{"name":"{a}","discriminator":[1],"accounts":[],"args":[]}},
                              {{"name":"{b}","discriminator":[2],"accounts":[],"args":[]}}]}}"#
        )
    };
    let colliding = idl("ABC", "Abc");
    assert_eq!(
        kinds(&colliding, &colliding),
        [(Severity::Breaking, ChangeKind::InstructionNameCollision)]
    );
    // Non-ASCII case stays apart in Shipstern's conversion, so this IDL compiles.
    let unicode = idl("ä", "Ä");
    assert!(kinds(&unicode, &unicode).is_empty());
}

#[test]
fn program_address_and_module_name_changes_are_reported() {
    let idl = |address: &str, name: &str| {
        format!(
            r#"{{"address":"{address}","metadata":{{"name":"{name}","version":"1","spec":"0.1.0"}},"instructions":[]}}"#
        )
    };
    assert_eq!(
        kinds(&idl("A", "lo"), &idl("B", "lo")),
        [(Severity::Breaking, ChangeKind::ProgramAddressChanged)]
    );
    // A legacy IDL can have no address.
    assert_eq!(kinds(&idl("", "lo"), &idl("B", "lo")), []);
    assert_eq!(
        kinds(&idl("A", "limit_order"), &idl("A", "limit_order_v2")),
        [(Severity::Dangerous, ChangeKind::ProgramRenamed)]
    );
    // Shipstern camel-cases, then snake-cases the name: both give the module `limit_order2`.
    assert_eq!(
        kinds(&idl("A", "limit_order_2"), &idl("A", "limitOrder2")),
        []
    );
}

#[test]
fn same_size_discriminator_is_a_collision() {
    // Shipstern groups size-only instructions by size, then tells them apart by account count.
    let sized = |name: &str| Instruction {
        size_discriminator: Some(4),
        ..ix(name, vec![], vec![acct("a")])
    };
    let old = bare_idl(vec![sized("only")]);
    let new = bare_idl(vec![sized("only"), sized("other")]);
    assert_eq!(
        idl_kinds(&old, &new),
        [
            (Severity::Breaking, ChangeKind::DiscriminatorReused),
            (Severity::Breaking, ChangeKind::Collision)
        ]
    );
    assert_eq!(
        idl_kinds(&new, &new),
        [(Severity::Dangerous, ChangeKind::CollisionPreexisting)]
    );
}

#[test]
fn account_types_with_one_size_discriminator_collide() {
    // Shipstern's account parser takes the first type whose size matches.
    let sized = |name: &str| NamedDisc {
        name: name.into(),
        discriminator: vec![],
        discriminator_offset: 0,
        extra_discriminators: vec![],
        size: Some(100),
        size_discriminator: Some(100),
        payload_offset: None,
    };
    let old = Idl {
        accounts: vec![sized("Pool")],
        ..empty_idl()
    };
    let new = Idl {
        accounts: vec![sized("Pool"), sized("Vault")],
        ..empty_idl()
    };
    assert_eq!(
        messages(&old, &new),
        [
            "unresolvable collision on account size discriminator 100 bytes: [Pool, Vault] all decode as Pool",
            "new account type: Vault"
        ]
    );
    assert_eq!(
        idl_kinds(&new, &new),
        [(Severity::Dangerous, ChangeKind::CollisionPreexisting)]
    );
}

#[test]
fn group_members_compare_by_the_names_shipstern_generates() {
    let group = |name: &str, accounts| AccountItem::Composite {
        name: name.into(),
        accounts,
    };
    let anchor = |accounts| bare_idl(vec![ix("grouped", vec![1], accounts)]);
    // `owner` repeats, so Shipstern's Anchor conversion prefixes each member with its group.
    let old = anchor(vec![
        acct("owner"),
        group("source", vec![acct("owner"), acct("token")]),
        group("destination", vec![acct("owner"), acct("token")]),
    ]);
    let codama = Idl {
        format: Format::Codama,
        ..bare_idl(vec![ix(
            "grouped",
            vec![1],
            [
                "owner",
                "sourceOwner",
                "sourceToken",
                "destinationOwner",
                "destinationToken",
            ]
            .map(acct)
            .to_vec(),
        )])
    };
    assert_eq!(idl_kinds(&old, &codama), []);

    let swapped = anchor(vec![
        acct("owner"),
        group("destination", vec![acct("owner"), acct("token")]),
        group("source", vec![acct("owner"), acct("token")]),
    ]);
    assert_eq!(
        idl_kinds(&old, &swapped),
        [(Severity::Breaking, ChangeKind::AccountMoved); 4]
    );
    // A new repeated name adds the prefix, but `token` still shifts to another position.
    // Between two Anchor IDLs the prefix keeps the IDL's own names.
    let before = anchor(vec![acct("owner"), group("source", vec![acct("token")])]);
    let after = anchor(vec![
        acct("owner"),
        group("source", vec![acct("owner"), acct("token")]),
    ]);
    assert_eq!(
        diff(&before, &after).changes[0].message,
        "account changed at fixed position (grouped#1): token -> source.owner"
    );
}
