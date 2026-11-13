//! Run a Carbon 2.0 decoder as the decoder under test of `idl-verify --decoder`.
//!
//! Add one binary to a crate that depends on your decoder crate, with its `serde` feature on:
//!
//! ```ignore
//! fn main() -> std::io::Result<()> {
//!     idl_verify_carbon::serve!(
//!         carbon_pumpfun_decoder::PumpfunDecoder,
//!         events = carbon_pumpfun_decoder::instructions::CpiEvent
//!     )
//! }
//! ```
//!
//! Then run `idl-verify --idl idl.json --decoder ./target/debug/<bin>`.
//!
//! The crate that calls `serve!` needs `carbon-core`, `solana-account` and `solana-instruction`
//! as direct dependencies, at the versions that the decoder uses: the macro names them.

/// Serve idl-verify's decoder protocol on stdin and stdout with a Carbon decoder.
///
/// - `serve!(decoder)`: one value that implements Carbon's `AccountDecoder` and
///   `InstructionDecoder`, as each generated decoder does. Events are skipped.
/// - `serve!(decoder, events = CpiEvent)`: also decode events with the `decode` function of
///   the decoder crate's `instructions::CpiEvent` type.
/// - `serve!(accounts = a, instructions = i)` and `serve!(accounts = a, instructions = i,
///   events = CpiEvent)`: two different decoders.
///
/// Each decoded type must implement `serde::Serialize`: turn on the decoder crate's `serde`
/// feature.
#[macro_export]
macro_rules! serve {
    (accounts = $accounts:expr, instructions = $instructions:expr, events = $events:ty $(,)?) => {
        $crate::__serve!($accounts, $instructions, (data) => $crate::__private::event(
            <$events>::decode(&$crate::__private::with_event_tag(&data))))
    };
    (accounts = $accounts:expr, instructions = $instructions:expr $(,)?) => {
        $crate::__serve!($accounts, $instructions, (_data) => $crate::__private::no_events())
    };
    ($decoder:expr, events = $events:ty $(,)?) => {{
        let decoder = $decoder;
        $crate::serve!(accounts = &decoder, instructions = &decoder, events = $events)
    }};
    ($decoder:expr $(,)?) => {{
        let decoder = $decoder;
        $crate::serve!(accounts = &decoder, instructions = &decoder)
    }};
}

#[doc(hidden)]
#[macro_export]
macro_rules! __serve {
    ($accounts:expr, $instructions:expr, ($data:ident) => $event:expr) => {{
        use ::carbon_core::account::AccountDecoder as _;
        use ::carbon_core::instruction::InstructionDecoder as _;
        let accounts = $accounts;
        let instructions = $instructions;
        $crate::__private::run(|request| match request {
            $crate::__private::Request::Account { program, data } => {
                let account = ::solana_account::Account {
                    lamports: 0,
                    data,
                    // A generated decoder skips an account that another program owns.
                    owner: ::core::convert::From::from(program),
                    executable: false,
                    rent_epoch: 0,
                };
                $crate::__private::account(
                    accounts
                        .decode_account(&account)
                        .map(|decoded| decoded.data),
                )
            }
            $crate::__private::Request::Instruction {
                program,
                keys,
                data,
            } => {
                let instruction = ::solana_instruction::Instruction {
                    program_id: ::core::convert::From::from(program),
                    accounts: keys
                        .into_iter()
                        .map(|key| ::solana_instruction::AccountMeta {
                            pubkey: ::core::convert::From::from(key),
                            is_signer: false,
                            is_writable: false,
                        })
                        .collect(),
                    data,
                };
                $crate::__private::instruction(instructions.decode_instruction(&instruction))
            }
            $crate::__private::Request::Event { data: $data } => $event,
        })
    }};
}

#[doc(hidden)]
pub mod __private {
    use std::any::Any;
    use std::io::{self, BufRead, Write};
    use std::panic::{self, AssertUnwindSafe};

    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use serde::Serialize;
    use serde_json::{json, Map, Value};

    /// Anchor's self-CPI event tag. idl-verify removes it, and `CpiEvent::decode` wants it.
    const EVENT_IX_TAG: [u8; 8] = [0xe4, 0x45, 0xa5, 0x2e, 0x51, 0xcb, 0x9a, 0x1d];

    /// Why events are skipped, not failed: a decoder without events still checks the rest.
    const NO_EVENT_TYPE: &str = "serve! got no event type; pass \
        `events = <decoder crate>::instructions::CpiEvent` as a second argument";

    /// One request of the protocol, with the keys as bytes.
    pub enum Request {
        Account {
            program: [u8; 32],
            data: Vec<u8>,
        },
        Instruction {
            program: [u8; 32],
            keys: Vec<[u8; 32]>,
            data: Vec<u8>,
        },
        /// `[discriminator][payload]`, without the self-CPI tag.
        Event {
            data: Vec<u8>,
        },
    }

    pub enum Reply {
        Ok(Value),
        Error(String),
        Skip(String),
    }

    impl Reply {
        fn to_json(&self) -> Value {
            match self {
                Reply::Ok(value) => json!({ "status": "ok", "value": value }),
                Reply::Error(error) => json!({ "status": "error", "error": error }),
                Reply::Skip(reason) => json!({ "status": "skip", "reason": reason }),
            }
        }
    }

    impl From<Result<Value, String>> for Reply {
        fn from(result: Result<Value, String>) -> Self {
            result.map_or_else(Reply::Error, Reply::Ok)
        }
    }

    /// # Errors
    ///
    /// When stdin or stdout fails.
    pub fn run(decode: impl FnMut(Request) -> Reply) -> io::Result<()> {
        serve(decode, io::stdin().lock(), io::stdout().lock())
    }

    fn serve(
        mut decode: impl FnMut(Request) -> Reply,
        input: impl BufRead,
        mut output: impl Write,
    ) -> io::Result<()> {
        for line in input.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            // A panic gets an error reply, so idl-verify still writes its report.
            let reply = match parse_request(&line) {
                Ok(request) => panic::catch_unwind(AssertUnwindSafe(|| decode(request)))
                    .unwrap_or_else(|payload| Reply::Error(panicked(payload.as_ref()))),
                Err(e) => Reply::Error(e),
            };
            writeln!(output, "{}", reply.to_json())?;
            output.flush()?;
        }
        Ok(())
    }

    fn panicked(payload: &(dyn Any + Send)) -> String {
        let message = payload
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
            .unwrap_or("no message");
        format!("the decoder panicked: {message}")
    }

    fn parse_request(line: &str) -> Result<Request, String> {
        let req: Value = serde_json::from_str(line).map_err(|e| format!("bad request: {e}"))?;
        let data = STANDARD
            .decode(req["data"].as_str().unwrap_or_default())
            .map_err(|e| format!("bad data: {e}"))?;
        let program = || {
            key(req["program"]
                .as_str()
                .ok_or("the request names no program; use a newer idl-verify")?)
        };
        match req["kind"].as_str() {
            Some("account") => Ok(Request::Account {
                program: program()?,
                data,
            }),
            Some("instruction") => Ok(Request::Instruction {
                program: program()?,
                keys: req["accounts"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                    .iter()
                    .map(|k| key(k.as_str().unwrap_or_default()))
                    .collect::<Result<_, _>>()?,
                data,
            }),
            Some("event") => Ok(Request::Event { data }),
            other => Err(format!("unknown kind {other:?}")),
        }
    }

    fn key(text: &str) -> Result<[u8; 32], String> {
        let bytes = bs58::decode(text)
            .into_vec()
            .map_err(|e| format!("bad key {text:?}: {e}"))?;
        bytes
            .try_into()
            .map_err(|_| format!("bad key {text:?}: not 32 bytes"))
    }

    #[must_use]
    pub fn with_event_tag(data: &[u8]) -> Vec<u8> {
        [&EVENT_IX_TAG[..], data].concat()
    }

    #[must_use]
    pub fn no_events() -> Reply {
        Reply::Skip(NO_EVENT_TYPE.into())
    }

    /// The fields of a decoded account. Carbon writes the account enum as
    /// `{"type": "<Name>", "data": {fields}}`.
    pub fn account<T: Serialize>(decoded: Option<T>) -> Reply {
        let Some(decoded) = decoded else {
            return Reply::Error("decode_account returned None: no account type matched".into());
        };
        to_json(&decoded)
            .and_then(split_variant)
            .map(|(_, fields)| fields)
            .into()
    }

    /// `{"name", "data"}` of a decoded instruction. Carbon writes the instruction enum as
    /// `{"type": "<Name>", "data": {"program_id", "data": {args}, "accounts"}}`.
    pub fn instruction<T: Serialize>(decoded: Option<T>) -> Reply {
        let Some(decoded) = decoded else {
            return Reply::Error(
                "decode_instruction returned None: no instruction matched the data and accounts"
                    .into(),
            );
        };
        to_json(&decoded)
            .and_then(split_variant)
            .and_then(|(name, body)| match body {
                Value::Object(mut body) => {
                    let args = empty_if_null(body.remove("data").unwrap_or_default());
                    Ok(json!({ "name": name, "data": args }))
                }
                other => Err(format!("unexpected instruction shape: {}", excerpt(&other))),
            })
            .into()
    }

    /// `{"name", "data"}` of a decoded event. Carbon writes `CpiEvent` as `{"<Name>": {fields}}`.
    pub fn event<T: Serialize>(decoded: Option<T>) -> Reply {
        let Some(decoded) = decoded else {
            return Reply::Error("CpiEvent::decode returned None: no event matched".into());
        };
        to_json(&decoded)
            .and_then(split_variant)
            .map(|(name, fields)| json!({ "name": name, "data": fields }))
            .into()
    }

    /// The JSON of `value`. `serde_json::to_value` rejects an integer above 64 bits without
    /// `arbitrary_precision`, and that feature would change `serde_json` for the whole build.
    fn to_json<T: Serialize>(value: &T) -> Result<Value, String> {
        let mut text = Vec::new();
        value
            .serialize(&mut serde_json::Serializer::with_formatter(
                &mut text,
                ExactNumbers::default(),
            ))
            .map_err(|e| format!("serialize: {e}"))?;
        serde_json::from_slice(&text).map_err(|e| format!("serialize: {e}"))
    }

    /// Writes an integer above 64 bits as a decimal string, as the protocol asks.
    #[derive(Default)]
    struct ExactNumbers {
        /// `serde_json` quotes a number key itself.
        in_key: bool,
    }

    impl serde_json::ser::Formatter for ExactNumbers {
        fn write_i128<W: ?Sized + Write>(&mut self, writer: &mut W, value: i128) -> io::Result<()> {
            if self.in_key || i64::try_from(value).is_ok() {
                write!(writer, "{value}")
            } else {
                write!(writer, "\"{value}\"")
            }
        }

        fn write_u128<W: ?Sized + Write>(&mut self, writer: &mut W, value: u128) -> io::Result<()> {
            if self.in_key || u64::try_from(value).is_ok() {
                write!(writer, "{value}")
            } else {
                write!(writer, "\"{value}\"")
            }
        }

        fn begin_object_key<W: ?Sized + Write>(
            &mut self,
            writer: &mut W,
            first: bool,
        ) -> io::Result<()> {
            self.in_key = true;
            if first {
                Ok(())
            } else {
                writer.write_all(b",")
            }
        }

        fn begin_object_value<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
            self.in_key = false;
            writer.write_all(b":")
        }
    }

    /// Name and body of a serde enum value: adjacently tagged `{"type": N, "data": D}`,
    /// externally tagged `{N: D}`, or a bare unit variant `"N"`.
    fn split_variant(value: Value) -> Result<(String, Value), String> {
        match value {
            Value::Object(mut map) if is_adjacent(&map) => match map.remove("type") {
                Some(Value::String(name)) => {
                    Ok((name, empty_if_null(map.remove("data").unwrap_or_default())))
                }
                _ => Err("unexpected enum shape: no variant name".into()),
            },
            Value::Object(map) if map.len() == 1 => map
                .into_iter()
                .next()
                .map(|(name, body)| (name, empty_if_null(body)))
                .ok_or_else(|| "unexpected enum shape: empty object".into()),
            Value::String(name) => Ok((name, json!({}))),
            other => Err(format!("unexpected enum shape: {}", excerpt(&other))),
        }
    }

    fn is_adjacent(map: &Map<String, Value>) -> bool {
        map.get("type").is_some_and(Value::is_string)
            && map.keys().all(|k| k == "type" || k == "data")
    }

    /// A struct with no fields (`struct Foo;`) serializes as `null`; idl-verify writes `{}`.
    fn empty_if_null(value: Value) -> Value {
        if value.is_null() {
            json!({})
        } else {
            value
        }
    }

    fn excerpt(value: &Value) -> String {
        value.to_string().chars().take(80).collect()
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use serde::Serialize;

        const PROGRAM: &str = "11111111111111111111111111111112";

        fn replies(input: &str, decode: impl FnMut(Request) -> Reply) -> Vec<Value> {
            let mut out = Vec::new();
            serve(decode, input.as_bytes(), &mut out).unwrap();
            String::from_utf8(out)
                .unwrap()
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect()
        }

        #[test]
        fn each_reply_has_the_protocol_status() {
            let input = format!(
                "{{\"kind\":\"account\",\"program\":\"{PROGRAM}\",\"address\":\"\",\"data\":\"AQI=\"}}\n\
                 \n\
                 {{\"kind\":\"instruction\",\"program\":\"{PROGRAM}\",\"data\":\"\",\"accounts\":[\"{PROGRAM}\"]}}\n\
                 {{\"kind\":\"event\",\"program\":\"{PROGRAM}\",\"source\":\"log\",\"data\":\"Bw==\"}}\n\
                 {{\"kind\":\"swap\",\"program\":\"{PROGRAM}\",\"data\":\"\"}}\n\
                 {{\"kind\":\"account\",\"data\":\"\"}}\n\
                 not json\n"
            );
            let got = replies(&input, |request| match request {
                Request::Account { program, data } => Reply::Ok(json!([program[31], data])),
                Request::Instruction { keys, .. } => Reply::Error(format!("{} keys", keys.len())),
                Request::Event { .. } => panic!("decoder bug"),
            });
            assert_eq!(got.len(), 6, "an empty line gets no reply: {got:?}");
            assert_eq!(got[0], json!({"status": "ok", "value": [1, [1, 2]]}));
            assert_eq!(got[1], json!({"status": "error", "error": "1 keys"}));
            // A panic in the decoder is an error reply, so idl-verify still writes its report.
            assert!(got[2]["error"].as_str().unwrap().contains("decoder bug"));
            for reply in &got[2..] {
                assert_eq!(reply["status"], "error", "{reply}");
            }
            assert!(got[4]["error"].as_str().unwrap().contains("no program"));
        }

        // The shapes of a generated Carbon decoder with its `serde` feature on.
        #[derive(Serialize)]
        #[serde(tag = "type", content = "data")]
        enum PoolAccount {
            Pool(Box<Pool>),
        }
        #[derive(Serialize)]
        struct Pool {
            fee: u16,
            liquidity: u128,
            price: f32,
        }
        #[derive(Serialize)]
        #[serde(tag = "type", content = "data")]
        enum PoolInstruction {
            Swap {
                program_id: [u8; 2],
                data: Swap,
                accounts: SwapAccounts,
            },
            Close {
                program_id: [u8; 2],
                data: Close,
                accounts: SwapAccounts,
            },
        }
        #[derive(Serialize)]
        struct Swap {
            cfg: Cfg,
        }
        #[derive(Serialize)]
        struct Cfg {
            cfg: u8,
            other: u8,
        }
        #[derive(Serialize)]
        struct Close;
        #[derive(Serialize)]
        struct SwapAccounts {
            pool: [u8; 2],
        }
        #[derive(Serialize)]
        enum CpiEvent {
            SwapEvent(SwapEvent),
        }
        #[derive(Serialize)]
        struct SwapEvent {
            delta: i128,
        }

        fn ok(reply: Reply) -> Value {
            match reply {
                Reply::Ok(value) => value,
                Reply::Error(e) | Reply::Skip(e) => panic!("not ok: {e}"),
            }
        }

        #[test]
        fn the_serde_envelope_is_unwrapped() {
            let pool = PoolAccount::Pool(Box::new(Pool {
                fee: 30,
                liquidity: 5,
                price: 0.5,
            }));
            assert_eq!(
                ok(account(Some(pool))),
                json!({"fee": 30, "liquidity": 5, "price": 0.5})
            );
            let swap = PoolInstruction::Swap {
                program_id: [1, 2],
                data: Swap {
                    cfg: Cfg { cfg: 1, other: 2 },
                },
                accounts: SwapAccounts { pool: [3, 4] },
            };
            // A lone struct argument stays under its name; idl-verify reads both forms.
            assert_eq!(
                ok(instruction(Some(swap))),
                json!({"name": "Swap", "data": {"cfg": {"cfg": 1, "other": 2}}})
            );
            let close = PoolInstruction::Close {
                program_id: [1, 2],
                data: Close,
                accounts: SwapAccounts { pool: [3, 4] },
            };
            assert_eq!(
                ok(instruction(Some(close))),
                json!({"name": "Close", "data": {}})
            );
            let ev = CpiEvent::SwapEvent(SwapEvent { delta: -3 });
            assert_eq!(
                ok(event(Some(ev))),
                json!({"name": "SwapEvent", "data": {"delta": -3}})
            );
        }

        #[test]
        fn wide_integers_become_strings() {
            let pool = PoolAccount::Pool(Box::new(Pool {
                fee: 1,
                liquidity: u128::MAX,
                price: 0.5,
            }));
            let fields = ok(account(Some(pool)));
            assert_eq!(fields["liquidity"], json!(u128::MAX.to_string()));
            let ev = CpiEvent::SwapEvent(SwapEvent { delta: i128::MIN });
            assert_eq!(
                ok(event(Some(ev)))["data"]["delta"],
                json!(i128::MIN.to_string())
            );
            let keys: std::collections::BTreeMap<u128, u8> = [(u128::MAX, 1)].into();
            assert_eq!(to_json(&keys).unwrap(), json!({ u128::MAX.to_string(): 1 }));
        }
    }
}
