pub mod constants;
pub mod error;
pub mod instructions;
pub mod state;

use anchor_lang::prelude::*;

pub use constants::*;
pub use instructions::*;
pub use state::*;

declare_id!("7cCT3NJUCV61oZkSpJvcH6d3nWaZsVjYW1YzwVoXGmLK");

#[program]
pub mod escrow_new {
    use super::*;

    pub fn make(ctx: Context<Make>, seed: u64, receive: u64, deposit: u64) -> Result<()> {
        ctx.accounts.init_escrow(seed, receive, &ctx.bumps)?;
        ctx.accounts.deposit(deposit)
    }
    pub fn take(ctx: Context<Take>) -> Result<()> {
        ctx.accounts.transfer()?;
        ctx.accounts.withdraw()?;
        ctx.accounts.close()
    }
    pub fn refund(ctx: Context<Refund>) -> Result<()> {
        ctx.accounts.withdraw()?;
        ctx.accounts.close()
    }

    pub fn update(ctx: Context<Update>, new_receive: u64) -> Result<()> {
        ctx.accounts.update(new_receive)
    }
}
