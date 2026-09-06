use anchor_lang::prelude::*;

#[error_code]
pub enum EscrowError {
    #[msg("Deadline must be in the future")]
    DeadlineInPast,
    #[msg("This escrow has expired and can only be refunded")]
    EscrowExpired,
    #[msg("Escrow terms changed since you read them")]
    TermsChanged,
}
