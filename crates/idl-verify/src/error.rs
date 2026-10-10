use std::path::PathBuf;

/// Errors that stop a run before any check can report (exit code 2).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot parse IDL {path}: {source}")]
    Idl {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("samples file is not valid JSON: {0}")]
    SamplesJson(serde_json::Error),
    #[error("{list}[{index}].data is not valid base64: {source}")]
    SampleData {
        list: &'static str,
        index: usize,
        source: base64::DecodeError,
    },
    #[error("cannot write the report: {0}")]
    Write(std::io::Error),
    #[error("reference file is not valid JSON: {0}")]
    ReferenceJson(serde_json::Error),
    #[error("reference file does not match the samples: {0}")]
    ReferenceMismatch(String),
    #[error("unexpected RPC response: {0}")]
    RpcResponse(String),
    #[error("RPC request failed: {0}")]
    Rpc(String),
}
