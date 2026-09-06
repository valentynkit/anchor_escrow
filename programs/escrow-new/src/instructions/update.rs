use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, TokenInterface};

use crate::{error::EscrowError, EscrowState, ESCROW_SEED};

#[derive(Accounts)]
pub struct Update<'info> {
    #[account()]
    pub maker: Signer<'info>,
    #[account(
        mint::token_program = token_program,
    )]
    pub mint_b: InterfaceAccount<'info, Mint>,
    #[account(
        mut, 
        has_one = maker,
        seeds = [ESCROW_SEED, escrow.maker.key().as_ref(), escrow.seed.to_le_bytes().as_ref()],
        bump = escrow.bump,
    )]
    pub escrow: Account<'info, EscrowState>,
    pub token_program: Interface<'info, TokenInterface>,
}

impl<'info> Update<'info> {
    pub fn update(&mut self, new_receive: u64) -> Result<()> {
        require_gt!(new_receive, 0);
        require_gt!(
            self.escrow.deadline,
            Clock::get()?.unix_timestamp,
            EscrowError::EscrowExpired
        );
        self.escrow.mint_b = self.mint_b.key();
        self.escrow.receive = new_receive;
        Ok(())
    }
}
