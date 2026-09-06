use crate::{EscrowState, ESCROW_SEED};
use anchor_lang::prelude::*;
use anchor_lang::{
    accounts::{account::Account, program::Program, signer::Signer},
    system_program::transfer,
    Accounts,
};
use anchor_spl::token_2022::{close_account, transfer_checked, CloseAccount, TransferChecked};
use anchor_spl::{
    associated_token::AssociatedToken,
    token_interface::{Mint, TokenAccount, TokenInterface},
};

#[derive(Accounts)]
pub struct Refund<'info> {
    #[account(mut)]
    pub maker: Signer<'info>,

    #[account(mut,
    associated_token::mint = mint_a,
    associated_token::authority = maker,
    associated_token::token_program = token_program,

    )]
    pub maker_ata_a: InterfaceAccount<'info, TokenAccount>,

    #[account(mint::token_program = token_program)]
    pub mint_a: InterfaceAccount<'info, Mint>,
    #[account(
        mut,
        seeds = [ESCROW_SEED, escrow.maker.key().as_ref(), escrow.seed.to_le_bytes().as_ref()],
        bump = escrow.bump,
        has_one = maker,
        has_one = mint_a,
        close = maker,
    )]
    pub escrow: Account<'info, EscrowState>,

    #[account(mut)]
    pub vault: InterfaceAccount<'info, TokenAccount>,
    pub token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

impl<'info> Refund<'info> {
    pub fn close(&mut self) -> Result<()> {
        let cpi_accounts = CloseAccount {
            account: self.vault.to_account_info(),
            destination: self.maker.to_account_info(),
            authority: self.escrow.to_account_info(),
        };

        let signer_seeds: [&[&[u8]]; 1] = [&[
            ESCROW_SEED,
            self.escrow.maker.as_ref(),
            &self.escrow.seed.to_le_bytes()[..],
            &[self.escrow.bump],
        ]];

        let cpi_ctx =
            CpiContext::new_with_signer(self.token_program.key(), cpi_accounts, &signer_seeds);

        close_account(cpi_ctx)?;
        Ok(())
    }
}
