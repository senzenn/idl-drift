# idl-drift

`idl-drift` compares two versions of a Solana program IDL. It puts each change in a tier,
by its effect on a parser that was generated from the old version: Breaking, Dangerous,
Additive or Cosmetic. It reads Anchor IDLs (0.30 spec and legacy) and Codama IDLs, and the
two files can use different formats.

To install the CLI and run the first commands, read the
[quick guide](https://github.com/senzenn/idl-drift/blob/main/QUICKGUIDE.md).

## Library

```rust
use idl_drift::{diff, model::Idl};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let old = Idl::from_json(&std::fs::read_to_string("old.json")?)?;
    let new = Idl::from_json(&std::fs::read_to_string("new.json")?)?;
    let report = diff(&old, &new);
    for change in &report.changes {
        println!("{:?} {:?} {}: {}", change.severity, change.kind, change.subject, change.message);
    }
    std::process::exit(report.exit_code());
}
```

## Documentation

- [idl-drift reference](https://github.com/senzenn/idl-drift/blob/main/docs/idl-drift.md):
  each rule, the rule names, the JSON format.
- [CI](https://github.com/senzenn/idl-drift/blob/main/docs/ci.md): GitHub Actions workflows.
- [idl-verify](https://github.com/senzenn/idl-drift/blob/main/docs/idl-verify.md), the
  companion tool that checks an IDL against real data.

## License

Licensed under either of Apache-2.0 or MIT, at your option.
