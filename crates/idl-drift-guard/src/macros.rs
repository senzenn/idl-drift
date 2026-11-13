/// Wrap a parser or decoder so that a [`Guard`](crate::Guard) counts each result. The wrapper
/// returns each value and each error unchanged.
///
/// - `guard!(shipstern, parser, guard)`: a Shipstern 0.12 `Parser` of accounts or
///   instructions. The wrapper keeps the parser's `id()` and prefilter, and is a
///   `ProgramParser` when the parser is one. The guard checks that program id at once.
/// - `guard!(vixen, parser, guard)`: the same for Vixen 0.7. Not Vixen 0.6: its parsers with
///   `program-events` turn an instruction error into `Filtered` or a value, so the guard
///   cannot see the miss.
/// - `guard!(carbon, decoder, guard)`: a Carbon 2.0 decoder. The wrapper is an
///   `AccountDecoder` and an `InstructionDecoder` as far as the decoder is one.
///
/// `guard` is a `Guard` or an `Arc<Guard>`. Share one `Arc<Guard>` between the account and
/// the instruction wrapper of a program.
#[macro_export]
macro_rules! guard {
    (shipstern, $parser:expr, $guard:expr $(,)?) => {
        $crate::__parser!(shipstern_core, $parser, $guard)
    };
    (vixen, $parser:expr, $guard:expr $(,)?) => {
        $crate::__parser!(yellowstone_vixen_core, $parser, $guard)
    };
    (carbon, $decoder:expr, $guard:expr $(,)?) => {
        $crate::__decoder!($decoder, $guard)
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __parser {
    // The arguments are bound before the items, so the items do not hide names in them.
    ($core:ident, $parser:expr, $guard:expr) => {
        match (
            $parser,
            ::core::convert::Into::<::std::sync::Arc<$crate::Guard>>::into($guard),
        ) {
            (parser, guard) => $crate::__parser!(@items $core, parser, guard),
        }
    };
    (@items $core:ident, $parser:ident, $guard:ident) => {{
        // A local trait: the guard crate cannot implement its own trait for these types.
        trait GuardInput {
            const KIND: $crate::Kind;
            fn update(&self) -> $crate::Update<'_>;
        }
        impl GuardInput for ::$core::AccountUpdate {
            const KIND: $crate::Kind = $crate::Kind::Account;
            fn update(&self) -> $crate::Update<'_> {
                match &self.account {
                    ::core::option::Option::Some(info) => $crate::Update {
                        program: ::core::option::Option::Some(&info.owner),
                        data: &info.data,
                        slot: ::core::option::Option::Some(self.slot),
                        key: ::core::option::Option::Some(&info.pubkey),
                    },
                    ::core::option::Option::None => $crate::Update {
                        slot: ::core::option::Option::Some(self.slot),
                        ..$crate::Update::default()
                    },
                }
            }
        }
        impl GuardInput for ::$core::instruction::InstructionUpdate {
            const KIND: $crate::Kind = $crate::Kind::Instruction;
            fn update(&self) -> $crate::Update<'_> {
                $crate::Update {
                    program: ::core::option::Option::Some(&self.program.0),
                    data: &self.data,
                    slot: ::core::option::Option::Some(self.shared.slot),
                    key: ::core::option::Option::Some(&self.shared.signature),
                }
            }
        }

        #[derive(Debug)]
        struct Guarded<P>(P, ::std::sync::Arc<$crate::Guard>);

        impl<P> ::$core::Parser for Guarded<P>
        where
            P: ::$core::Parser + ::core::marker::Sync,
            P::Input: GuardInput + ::core::marker::Sync,
        {
            type Input = P::Input;
            type Output = P::Output;

            fn id(&self) -> ::std::borrow::Cow<'static, str> {
                ::$core::Parser::id(&self.0)
            }

            fn prefilter(&self) -> ::$core::Prefilter {
                ::$core::Parser::prefilter(&self.0)
            }

            async fn parse(&self, value: &Self::Input) -> ::$core::ParseResult<Self::Output> {
                let result = ::$core::Parser::parse(&self.0, value).await;
                let verdict = match &result {
                    ::core::result::Result::Ok(_) => $crate::Verdict::Decoded,
                    ::core::result::Result::Err(::$core::ParseError::Filtered) => {
                        $crate::Verdict::Filtered
                    }
                    ::core::result::Result::Err(::$core::ParseError::DiscriminatorNotFound(_)) => {
                        $crate::Verdict::UnknownDiscriminator
                    }
                    ::core::result::Result::Err(_) => $crate::Verdict::Failed,
                };
                self.1.observe(
                    <P::Input as GuardInput>::KIND,
                    verdict,
                    &GuardInput::update(value),
                );
                result
            }
        }

        impl<P> ::$core::ProgramParser for Guarded<P>
        where
            P: ::$core::ProgramParser + ::core::marker::Sync,
            P::Input: GuardInput + ::core::marker::Sync,
        {
            fn program_id(&self) -> ::$core::Pubkey {
                ::$core::ProgramParser::program_id(&self.0)
            }
        }

        // Autoref selects `Program` for a `ProgramParser`, and `NoProgram` for another parser.
        struct Probe<'a, T>(&'a T);
        trait Program {
            fn program(&self) -> ::core::option::Option<[u8; 32]>;
        }
        impl<T: ::$core::ProgramParser> Program for Probe<'_, T> {
            fn program(&self) -> ::core::option::Option<[u8; 32]> {
                ::core::option::Option::Some(::$core::ProgramParser::program_id(self.0).0)
            }
        }
        trait NoProgram {
            fn program(&self) -> ::core::option::Option<[u8; 32]>;
        }
        impl<T> NoProgram for &Probe<'_, T> {
            fn program(&self) -> ::core::option::Option<[u8; 32]> {
                ::core::option::Option::None
            }
        }
        if let ::core::option::Option::Some(program) = (&Probe(&$parser)).program() {
            $guard.check_program(&program);
        }

        Guarded($parser, $guard)
    }};
}

#[doc(hidden)]
#[macro_export]
macro_rules! __decoder {
    // The arguments are bound before the items, so the items do not hide names in them.
    ($decoder:expr, $guard:expr) => {
        match (
            $decoder,
            ::core::convert::Into::<::std::sync::Arc<$crate::Guard>>::into($guard),
        ) {
            (decoder, guard) => $crate::__decoder!(@items decoder, guard),
        }
    };
    (@items $decoder:ident, $guard:ident) => {{
        struct Guarded<D>(D, ::std::sync::Arc<$crate::Guard>);

        impl<'a, D> ::carbon_core::account::AccountDecoder<'a> for Guarded<D>
        where
            D: ::carbon_core::account::AccountDecoder<'a>,
        {
            type AccountType = D::AccountType;

            fn decode_account(
                &self,
                account: &'a ::solana_account::Account,
            ) -> ::core::option::Option<::carbon_core::account::DecodedAccount<Self::AccountType>>
            {
                let decoded =
                    ::carbon_core::account::AccountDecoder::decode_account(&self.0, account);
                let verdict = if decoded.is_some() {
                    $crate::Verdict::Decoded
                } else {
                    $crate::Verdict::Failed
                };
                // Carbon decoders do not see the slot or the account address.
                self.1.observe(
                    $crate::Kind::Account,
                    verdict,
                    &$crate::Update {
                        program: ::core::option::Option::Some(
                            ::core::convert::AsRef::<[u8]>::as_ref(&account.owner),
                        ),
                        data: &account.data,
                        ..$crate::Update::default()
                    },
                );
                decoded
            }
        }

        impl<'a, D> ::carbon_core::instruction::InstructionDecoder<'a> for Guarded<D>
        where
            D: ::carbon_core::instruction::InstructionDecoder<'a>,
        {
            type InstructionType = D::InstructionType;

            fn decode_instruction(
                &self,
                instruction: &'a ::solana_instruction::Instruction,
            ) -> ::core::option::Option<Self::InstructionType> {
                let decoded = ::carbon_core::instruction::InstructionDecoder::decode_instruction(
                    &self.0,
                    instruction,
                );
                let verdict = if decoded.is_some() {
                    $crate::Verdict::Decoded
                } else {
                    $crate::Verdict::Failed
                };
                self.1.observe(
                    $crate::Kind::Instruction,
                    verdict,
                    &$crate::Update {
                        program: ::core::option::Option::Some(
                            ::core::convert::AsRef::<[u8]>::as_ref(&instruction.program_id),
                        ),
                        data: &instruction.data,
                        ..$crate::Update::default()
                    },
                );
                decoded
            }
        }

        Guarded($decoder, $guard)
    }};
}
