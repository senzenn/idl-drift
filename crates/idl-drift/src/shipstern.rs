//! IDLs that are valid Codama, but that Shipstern's generated parser cannot use.
//!
//! Shipstern selects an instruction or an account type by its first discriminator only.

use codama_nodes::{AccountNode, DiscriminatorNode, NestedTypeNode, Number, TypeNode, ValueNode};

use crate::diff::collisions::collision_classes;
use crate::model::Idl;

/// Instructions that Shipstern cannot tell apart: the same first discriminator and the same
/// account count. Shipstern rejects each of them as ambiguous at run time.
#[must_use]
pub fn ambiguous_instructions(idl: &Idl) -> Vec<String> {
    collision_classes(&idl.instructions)
        .into_iter()
        .filter(|(_, names)| names.len() >= 2)
        .map(|((key, count), names)| {
            let names: Vec<String> = names.into_iter().collect();
            format!("{key} with {count} accounts: [{}]", names.join(", "))
        })
        .collect()
}

/// Codama account types whose first discriminator Shipstern cannot read, with the reason.
/// Shipstern skips such a type, so it reports each account of it as unknown.
///
/// # Errors
///
/// When `json` is not JSON, or a Codama root that does not parse.
pub fn skipped_accounts(json: &str) -> Result<Vec<(String, &'static str)>, serde_json::Error> {
    let mut value: serde_json::Value = serde_json::from_str(json)?;
    if value.get("kind").and_then(|k| k.as_str()) != Some("rootNode") {
        // Shipstern converts an Anchor IDL itself, with byte discriminators.
        return Ok(Vec::new());
    }
    crate::codama_convert::fix_string_error_codes(&mut value);
    let root: codama_nodes::RootNode = serde_json::from_value(value)?;
    Ok(root
        .program
        .accounts
        .iter()
        .filter_map(|a| skip_reason(a).map(|why| (a.name.to_string(), why)))
        .collect())
}

/// The same checks as Shipstern's account parser. An account with no discriminator is left
/// out: idl-verify already reports that it is picked by size.
fn skip_reason(account: &AccountNode) -> Option<&'static str> {
    match account.discriminators.first()? {
        DiscriminatorNode::Size(_) => None,
        DiscriminatorNode::Constant(c) => match c.constant.value.as_ref() {
            ValueNode::Number(n) if matches!(n.number, Number::UnsignedInteger(_)) => None,
            ValueNode::Bytes(_) => None,
            _ => Some("the constant discriminator is not an unsigned number or bytes"),
        },
        DiscriminatorNode::Field(f) => {
            let NestedTypeNode::Value(data) = &account.data else {
                return Some("the account data is not a plain struct");
            };
            let Some(field) = data.fields.iter().find(|x| x.name == f.name) else {
                return Some("the discriminator field is not in the account data");
            };
            if !matches!(*field.r#type, TypeNode::FixedSize(_)) {
                return Some("the discriminator field is not fixed-size bytes");
            }
            match &*field.default_value {
                Some(ValueNode::Bytes(_)) => None,
                _ => Some("the discriminator field has no bytes default value"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Codama root with these instructions and accounts.
    fn root(instructions: &str, accounts: &str) -> String {
        format!(
            r#"{{"kind":"rootNode","standard":"codama","version":"1.0.0","program":{{
            "kind":"programNode","name":"t","publicKey":"P","version":"0","errors":[],
            "definedTypes":[],"pdas":[],"instructions":[{instructions}],"accounts":[{accounts}]}}}}"#
        )
    }

    /// An instruction with tag `tag`, `accounts` accounts, and a size discriminator.
    fn ix(name: &str, tag: u8, accounts: usize, size: usize) -> String {
        let account =
            r#"{"kind":"instructionAccountNode","name":"a","isWritable":false,"isSigner":false}"#;
        format!(
            r#"{{"kind":"instructionNode","name":"{name}","accounts":[{}],"arguments":[
            {{"kind":"instructionArgumentNode","name":"discriminator","type":{{"kind":"numberTypeNode","format":"u8","endian":"le"}},
            "defaultValue":{{"kind":"numberValueNode","number":{tag}}},"defaultValueStrategy":"omitted"}}],
            "discriminators":[{{"kind":"fieldDiscriminatorNode","name":"discriminator","offset":0}},
            {{"kind":"sizeDiscriminatorNode","size":{size}}}]}}"#,
            vec![account; accounts].join(",")
        )
    }

    /// An account type whose field discriminator has type `ty` and default `default`.
    fn account(name: &str, ty: &str, default: &str) -> String {
        format!(
            r#"{{"kind":"accountNode","name":"{name}","data":{{"kind":"structTypeNode","fields":[
            {{"kind":"structFieldTypeNode","name":"discriminator","type":{ty},"defaultValue":{default}}}]}},
            "discriminators":[{{"kind":"fieldDiscriminatorNode","name":"discriminator","offset":0}}]}}"#
        )
    }

    #[test]
    fn same_first_discriminator_and_account_count_is_ambiguous() {
        // One tag at two data lengths: only the size discriminator tells them apart.
        let json = root(
            &[
                ix("swap", 7, 13, 18),
                ix("swapPadded", 7, 13, 25),
                ix("close", 7, 2, 1),
            ]
            .join(","),
            "",
        );
        let idl = Idl::from_json(&json).unwrap();
        assert_eq!(
            ambiguous_instructions(&idl),
            ["discriminator 07 with 13 accounts: [swap, swapPadded]"]
        );
    }

    #[test]
    fn a_field_discriminator_must_be_fixed_size_bytes() {
        let bytes = r#"{"kind":"fixedSizeTypeNode","size":8,"type":{"kind":"bytesTypeNode"}}"#;
        let default = r#"{"kind":"bytesValueNode","encoding":"base16","data":"fc00000000000000"}"#;
        let number = r#"{"kind":"numberTypeNode","format":"u64","endian":"le"}"#;
        let json = root(
            "",
            &[
                account("anchorStyle", bytes, default),
                account(
                    "numberField",
                    number,
                    r#"{"kind":"numberValueNode","number":252}"#,
                ),
            ]
            .join(","),
        );
        assert_eq!(
            skipped_accounts(&json).unwrap(),
            [(
                "numberField".to_string(),
                "the discriminator field is not fixed-size bytes"
            )]
        );
        assert!(
            skipped_accounts(r#"{"address":"P","metadata":{"spec":"0.1.0"}}"#)
                .unwrap()
                .is_empty()
        );
    }
}
