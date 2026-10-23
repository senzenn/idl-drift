use anchor_lang::prelude::*;

#[program]
pub mod vault {
    use super::*;

    pub fn deposit(_ctx: Context<Deposit>) -> Result<()> {
        Ok(())
    }
}

#[derive(Accounts)]
pub struct Deposit {}
