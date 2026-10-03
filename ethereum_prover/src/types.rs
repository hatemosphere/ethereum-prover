use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum ProofSecurity {
    #[serde(rename = "security_80")]
    #[value(name = "security_80")]
    Security80,
    #[serde(rename = "security_100")]
    #[value(name = "security_100")]
    Security100,
}

impl ProofSecurity {
    pub fn proof_wire_value(self) -> u8 {
        match self {
            Self::Security80 => 80,
            Self::Security100 => 100,
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    CpuWitness,
    GpuProve,
}

#[derive(Debug, Deserialize, Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum CachePolicy {
    Off,
    OnFailure,
    Always,
}

#[derive(Debug, Deserialize, Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum EthProofsSubmission {
    Off,
    /// Run the whole submission path but write each request to `<data_dir>/dry-run/`
    /// instead of sending it.
    DryRun,
    Staging,
    Prod,
}

impl EthProofsSubmission {
    pub fn enabled(&self) -> bool {
        match self {
            EthProofsSubmission::Off => false,
            EthProofsSubmission::DryRun
            | EthProofsSubmission::Staging
            | EthProofsSubmission::Prod => true,
        }
    }

    pub fn is_staging(&self) -> bool {
        matches!(self, EthProofsSubmission::Staging)
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum OnFailure {
    Exit,
    Continue,
}
