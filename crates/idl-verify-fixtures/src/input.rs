//! The arguments of `sample_tests!`.

use proc_macro2::Span;
use syn::parse::{Parse, ParseStream};
use syn::{Expr, Ident, LitStr, Token};

use crate::scan::Framework;

pub(crate) struct Input {
    pub(crate) framework: Framework,
    pub(crate) dir: LitStr,
    pub(crate) accounts: Option<Expr>,
    pub(crate) instructions: Option<Expr>,
    pub(crate) expected: Option<LitStr>,
}

impl Parse for Input {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let name: Ident = input.parse()?;
        let framework = match name.to_string().as_str() {
            "shipstern" => Framework::Shipstern,
            "carbon" => Framework::Carbon,
            _ => {
                return Err(syn::Error::new(
                    name.span(),
                    "expected `shipstern` or `carbon`",
                ))
            }
        };
        let mut dir = None;
        let mut accounts = None;
        let mut instructions = None;
        let mut expected = None;
        while !input.is_empty() {
            input.parse::<Token![,]>()?;
            if input.is_empty() {
                break;
            }
            let key: Ident = input.parse()?;
            input.parse::<Token![:]>()?;
            match key.to_string().as_str() {
                "dir" => set(&mut dir, &key, input.parse()?)?,
                "accounts" => set(&mut accounts, &key, input.parse()?)?,
                "instructions" => set(&mut instructions, &key, input.parse()?)?,
                "expected" => set(&mut expected, &key, input.parse()?)?,
                _ => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!(
                            "unknown key `{key}`; expected `dir`, `accounts`, `instructions` or `expected`"
                        ),
                    ))
                }
            }
        }
        let dir = dir.ok_or_else(|| {
            syn::Error::new(
                Span::call_site(),
                "missing `dir: \"<path>\"`, the fixture directory from the crate root",
            )
        })?;
        if accounts.is_none() && instructions.is_none() {
            return Err(syn::Error::new(
                Span::call_site(),
                "give `accounts: <parser>`, `instructions: <parser>` or both",
            ));
        }
        Ok(Self {
            framework,
            dir,
            accounts,
            instructions,
            expected,
        })
    }
}

fn set<T>(slot: &mut Option<T>, key: &Ident, value: T) -> syn::Result<()> {
    if slot.replace(value).is_some() {
        return Err(syn::Error::new(
            key.span(),
            format!("`{key}` is given twice"),
        ));
    }
    Ok(())
}
