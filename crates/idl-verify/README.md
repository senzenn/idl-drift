# idl-verify

`idl-verify` checks a Solana program IDL against real account, instruction and event data.
It classifies each sample by discriminator, decodes it from the IDL alone, and reports the
data that does not fit: unknown discriminators, data that does not decode, bytes that the
IDL does not read (such as zero-copy padding that a converter removed), and instructions
whose accounts do not match the IDL. It reads Anchor IDLs (0.30 spec and legacy) and Codama
IDLs.

To install the CLI and run the first commands, read the
[quick guide](https://github.com/senzenn/idl-drift/blob/main/QUICKGUIDE.md).

## Library

```rust
use idl_drift::model::Idl;
use idl_verify::{verify, Samples};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let idl = Idl::from_json(&std::fs::read_to_string("idl.json")?)?;
    let samples = Samples::from_json(&std::fs::read_to_string("samples.json")?)?;
    let report = verify(&idl, &samples, None);
    for finding in &report.findings {
        println!("{:?} {}: {}", finding.severity, finding.subject, finding.message);
    }
    std::process::exit(i32::from(report.exit_code()));
}
```

## Documentation

- [idl-verify reference](https://github.com/senzenn/idl-drift/blob/main/docs/idl-verify.md):
  each check, the samples format, live sampling, the JSON format, limitations.
- [Decoder protocol](https://github.com/senzenn/idl-drift/blob/main/docs/decoder-protocol.md)
  for `--decoder`.
- [CI](https://github.com/senzenn/idl-drift/blob/main/docs/ci.md): GitHub Actions workflows.

## License

Licensed under either of Apache-2.0 or MIT, at your option.
