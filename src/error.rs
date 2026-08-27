
#[derive(thiserror::Error, Debug)]
pub enum ClinixError {
    #[error("`{command}` is not implemented yet ({tracking_note})")]
    NotImplemented { command: String, tracking_note: &'static str },

		#[error("invalid environment id `{0}` — expected `scope:name` (scope ∈ sys|user|dev|run)")]
    InvalidEnvId(String),

    #[error("invalid package spec `{0}` — expected `name` or `name=version`")]
    InvalidPackage(String),
}

pub fn NotImplemented(command: &str, tracking_note: &'static str) -> ClinixError {
	NotImplemented("Not yet implemented.")
}
