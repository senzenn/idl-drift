//! A hand-written Vixen 0.7 parser of each input type, through `guard!(vixen, ..)`.

use std::borrow::Cow;
use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use idl_drift_guard::{guard, Guard};
use yellowstone_vixen_core::instruction::{InstructionShared, InstructionUpdate};
use yellowstone_vixen_core::{
    AccountUpdate, AccountUpdateInfo, KeyBytes, ParseError, ParseResult, Parser, Prefilter,
    ProgramParser, Pubkey,
};

const PROGRAM: [u8; 32] = [5; 32];

/// Decodes 8 bytes of an account of `PROGRAM` as a `u64`.
#[derive(Debug, Clone, Copy)]
struct Accounts;

impl Parser for Accounts {
    type Input = AccountUpdate;
    type Output = u64;

    fn id(&self) -> Cow<'static, str> {
        "accounts".into()
    }

    fn prefilter(&self) -> Prefilter {
        Prefilter::default()
    }

    async fn parse(&self, update: &AccountUpdate) -> ParseResult<u64> {
        let info = update.account.as_ref().ok_or(ParseError::Filtered)?;
        if info.owner != PROGRAM {
            return Err(ParseError::Filtered);
        }
        let bytes: [u8; 8] = info.data.as_slice().try_into().map_err(|_| "not 8 bytes")?;
        Ok(u64::from_le_bytes(bytes))
    }
}

impl ProgramParser for Accounts {
    fn program_id(&self) -> Pubkey {
        KeyBytes(PROGRAM)
    }
}

/// Knows one instruction, tag 1. Not a `ProgramParser`.
#[derive(Debug, Clone, Copy)]
struct Instructions;

impl Parser for Instructions {
    type Input = InstructionUpdate;
    type Output = u8;

    fn id(&self) -> Cow<'static, str> {
        "instructions".into()
    }

    fn prefilter(&self) -> Prefilter {
        Prefilter::default()
    }

    async fn parse(&self, update: &InstructionUpdate) -> ParseResult<u8> {
        match update.data.first() {
            Some(1) => Ok(1),
            _ => Err(ParseError::DiscriminatorNotFound("no tag".into())),
        }
    }
}

fn ready<F: Future>(future: F) -> F::Output {
    match pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(output) => output,
        Poll::Pending => panic!("the parser waited"),
    }
}

fn account(owner: [u8; 32], data: Vec<u8>) -> AccountUpdate {
    AccountUpdate {
        account: Some(AccountUpdateInfo {
            owner: owner.to_vec(),
            data,
            ..Default::default()
        }),
        slot: 3,
        ..Default::default()
    }
}

fn instruction(data: Vec<u8>) -> InstructionUpdate {
    InstructionUpdate {
        program: KeyBytes(PROGRAM),
        accounts: Vec::new(),
        data,
        shared: Arc::new(InstructionShared::default()),
        inner: Vec::new(),
        path: vec![0u32].into(),
        log_range: 0..0,
    }
}

#[test]
fn guard_wraps_vixen_0_7_parsers() {
    let program = Arc::new(Guard::new(PROGRAM, "p"));
    let accounts = guard!(vixen, Accounts, program.clone());
    let instructions = guard!(vixen, Instructions, program.clone());
    assert_eq!(accounts.id(), "accounts");
    assert_eq!(ProgramParser::program_id(&accounts).0, PROGRAM);
    assert_eq!(program.program_mismatch(), None);

    assert_eq!(
        ready(accounts.parse(&account(PROGRAM, 7u64.to_le_bytes().to_vec()))).ok(),
        Some(7)
    );
    assert!(ready(accounts.parse(&account(PROGRAM, vec![1, 2, 3]))).is_err());
    assert!(matches!(
        ready(accounts.parse(&account([6; 32], vec![0; 8]))),
        Err(ParseError::Filtered)
    ));
    assert_eq!(
        ready(instructions.parse(&instruction(vec![1]))).ok(),
        Some(1)
    );
    assert!(ready(instructions.parse(&instruction(vec![2]))).is_err());

    let counts = program.snapshot();
    assert_eq!(
        (
            counts.accounts.decoded,
            counts.accounts.decode_error,
            counts.accounts.filtered
        ),
        (1, 1, 1)
    );
    assert_eq!(
        (
            counts.instructions.decoded,
            counts.instructions.unknown_discriminator
        ),
        (1, 1)
    );

    let wrong = Arc::new(Guard::new([9; 32], "wrong"));
    let _accounts = guard!(vixen, Accounts, wrong.clone());
    assert_eq!(wrong.program_mismatch(), Some(&PROGRAM));
}
