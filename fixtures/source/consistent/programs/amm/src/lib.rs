use anchor_lang::prelude::*;

// The id before the keypair was regenerated:
// declare_id!("3JFwZkvgfoNvJdpjhJ7uYMnVzwaTRnyezYeZ1JSq9iLG");
declare_id!("29dv8e1WcjL4w6a7HDaHbUfXrF12yiJiVcKQ1qgeT3rF");

#[program]
pub mod amm {
    use super::*;

    pub fn initialize(_ctx: Context<Initialize>) -> Result<()> {
        Ok(())
    }
}

#[derive(Accounts)]
pub struct Initialize {}
