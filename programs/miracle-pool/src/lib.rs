use anchor_lang::prelude::*;
use anchor_lang::system_program;

declare_id!("3Kats7w6uuhmA4sXj852pFaCJ3DbvfgwNJKJVALwLPrU");

const MAX_LAB_ATTEMPTS: u8 = 15;
/// Max gacha rolls per `deposit_gacha_flat` ix вЂ” must match `GACHA_ROLLS_MAX_ONE_ACTION` in `@game/shared`.
const MAX_GACHA_ROLLS_PER_IX: u16 = 2000;
/// One gacha action fee covers 1..100 rolls; 101..200 rolls costs two fee units, etc.
const GACHA_ROLLS_PER_FEE_UNIT: u64 = 100;
/// Sixth mining rig slot unlock (0.5 SOL) вЂ” must match `MIRACLE_SOL_POOL_EXTRA_MINING_RIG_LAMPORTS` in `@game/shared`.
const MINING_EXTRA_RIG_SLOT_LAMPORTS: u64 = 500_000_000;
/// NFT inventory +100 slots bundle (0.1 SOL) вЂ” must match `MIRACLE_SOL_POOL_INVENTORY_SLOT_BUNDLE_LAMPORTS` in `@game/shared`.
const INVENTORY_SLOT_BUNDLE_LAMPORTS: u64 = 100_000_000;
const BPS_DENOMINATOR: u64 = 10_000;
const DEV_FEE_BPS: u64 = 500;
const ACTION_GACHA: u8 = 0;
const ACTION_LAB: u8 = 1;
const ACTION_CONTRIBUTE: u8 = 2;
const ACTION_MINING_EXTRA_RIG: u8 = 3;
const ACTION_INVENTORY_SLOT_BUNDLE: u8 = 4;
const ACTION_NFT_BURN: u8 = 5;
const ACTION_CITY_UPGRADE: u8 = 6;
const ACTION_GUILD_PROJECT_UPGRADE: u8 = 7;

#[account]
pub struct PoolState {
    pub authority: Pubkey,
    pub action_fee_lamports: u64,
    pub reserved_season_bps: u16,
    pub reserved_daily_bps: u16,
    pub bump_state: u8,
    pub bump_vault: u8,
    /// Deposit-time protocol fee recipient. Payment instructions send 5% here and 95% to the target vault.
    pub dev_fee_recipient: Pubkey,
}

impl PoolState {
    pub const SPACE: usize = 8 + 32 + 8 + 2 + 2 + 1 + 1 + 32;
}

#[account]
pub struct SubVaultMarker {}

impl SubVaultMarker {
    pub const SPACE: usize = 8;
}

#[program]
pub mod miracle_pool {
    use super::*;

    pub fn initialize(
        ctx: Context<Initialize>,
        action_fee_lamports: u64,
        reserved_season_bps: u16,
        reserved_daily_bps: u16,
        dev_fee_recipient: Pubkey,
    ) -> Result<()> {
        require!(dev_fee_recipient != Pubkey::default(), ErrorCode::BadDevRecipient);
        let s = &mut ctx.accounts.pool_state;
        s.authority = ctx.accounts.authority.key();
        s.action_fee_lamports = action_fee_lamports;
        s.reserved_season_bps = reserved_season_bps;
        s.reserved_daily_bps = reserved_daily_bps;
        s.bump_state = ctx.bumps.pool_state;
        s.bump_vault = ctx.bumps.pool_vault;
        s.dev_fee_recipient = dev_fee_recipient;
        Ok(())
    }

    /// Authority-only admin rotation. The new authority must be a non-default pubkey.
    pub fn set_authority(ctx: Context<SetAuthority>, new_authority: Pubkey) -> Result<()> {
        require!(new_authority != Pubkey::default(), ErrorCode::BadAuthority);
        ctx.accounts.pool_state.authority = new_authority;
        Ok(())
    }

    /// Authority-only action fee update.
    pub fn set_action_fee(ctx: Context<SetAuthority>, new_action_fee_lamports: u64) -> Result<()> {
        require!(new_action_fee_lamports > 0, ErrorCode::ZeroAmount);
        ctx.accounts.pool_state.action_fee_lamports = new_action_fee_lamports;
        Ok(())
    }

    /// Authority-only dev fee recipient rotation. The new recipient must be a non-default pubkey.
    pub fn set_dev_fee_recipient(
        ctx: Context<SetAuthority>,
        new_dev_fee_recipient: Pubkey,
    ) -> Result<()> {
        require!(
            new_dev_fee_recipient != Pubkey::default(),
            ErrorCode::BadDevRecipient
        );
        ctx.accounts.pool_state.dev_fee_recipient = new_dev_fee_recipient;
        Ok(())
    }

    /// Unified main-pool payment. `action` selects the off-chain game action; the server verifies args + tx deltas.
    pub fn deposit_action(
        ctx: Context<DepositFlat>,
        action: u8,
        units: u16,
        lamports: u64,
    ) -> Result<()> {
        let fee = main_pool_action_lamports(&ctx.accounts.pool_state, action, units, lamports)?;
        transfer_user_to_vault_with_dev_fee(
            &ctx.accounts.user,
            ctx.accounts.pool_vault.to_account_info(),
            ctx.accounts.dev_fee_recipient.to_account_info(),
            &ctx.accounts.system_program,
            fee,
        )?;
        Ok(())
    }

    /// Match-local PvP add-on payment. Funds go to a duel-specific vault, not the main pool.
    pub fn deposit_pvp_session_addon(
        ctx: Context<DepositPvpSessionAddon>,
        _session_key: [u8; 32],
        lamports: u64,
    ) -> Result<()> {
        require!(lamports > 0, ErrorCode::ZeroAmount);
        transfer_user_to_vault_with_dev_fee(
            &ctx.accounts.user,
            ctx.accounts.pvp_addon_vault.to_account_info(),
            ctx.accounts.dev_fee_recipient.to_account_info(),
            &ctx.accounts.system_program,
            lamports,
        )?;
        Ok(())
    }

    /// Move the protocol fee from a match-local PvP add-on vault to the main pool vault.
    /// The server computes the exact fee from DB rows; the on-chain authority submits that amount.
    pub fn pvp_addon_skim_fee_to_main(
        ctx: Context<PvpAddonSkimFeeToMain>,
        session_key: [u8; 32],
        lamports: u64,
    ) -> Result<()> {
        require!(lamports > 0, ErrorCode::ZeroAmount);
        require!(
            vault_net_lamports(&ctx.accounts.pvp_addon_vault.to_account_info())? >= lamports,
            ErrorCode::PvpVaultInsufficientForFeeSkim
        );
        let bump_arr = [ctx.bumps.pvp_addon_vault];
        let vault_seeds: &[&[u8]] = &[
            b"mirl".as_ref(),
            b"pvpaddon".as_ref(),
            session_key.as_ref(),
            bump_arr.as_ref(),
        ];
        transfer_vault_to_dest_signed(
            ctx.accounts.pvp_addon_vault.to_account_info(),
            ctx.accounts.pool_vault.to_account_info(),
            &ctx.accounts.system_program,
            lamports,
            &[vault_seeds],
        )?;
        Ok(())
    }

    /// Authority pays duel winners from the match-local PvP add-on vault.
    pub fn pvp_addon_authority_withdraw(
        ctx: Context<PvpAddonAuthorityWithdraw>,
        session_key: [u8; 32],
        lamports: u64,
    ) -> Result<()> {
        require!(lamports > 0, ErrorCode::ZeroAmount);
        let bump_arr = [ctx.bumps.pvp_addon_vault];
        let vault_seeds: &[&[u8]] = &[
            b"mirl".as_ref(),
            b"pvpaddon".as_ref(),
            session_key.as_ref(),
            bump_arr.as_ref(),
        ];
        transfer_vault_to_dest_signed(
            ctx.accounts.pvp_addon_vault.to_account_info(),
            ctx.accounts.destination.to_account_info(),
            &ctx.accounts.system_program,
            lamports,
            &[vault_seeds],
        )?;
        Ok(())
    }

    /// Close an empty match-local PvP add-on vault after fee skim + winner payouts.
    pub fn pvp_addon_close(ctx: Context<PvpAddonClose>, _session_key: [u8; 32]) -> Result<()> {
        let net = vault_net_lamports(&ctx.accounts.pvp_addon_vault.to_account_info())?;
        require!(net == 0, ErrorCode::PvpVaultNotEmpty);
        Ok(())
    }

    /// Authority-only payout (MVP devnet): move lamports from main vault to any recipient.
    pub fn authority_send(ctx: Context<AuthoritySend>, lamports: u64) -> Result<()> {
        require!(lamports > 0, ErrorCode::ZeroAmount);
        let bump = ctx.accounts.pool_state.bump_vault;
        let bump_arr = [bump];
        let vault_seeds: &[&[u8]] = &[b"mirl".as_ref(), b"vault".as_ref(), bump_arr.as_ref()];
        transfer_vault_to_dest_signed(
            ctx.accounts.pool_vault.to_account_info(),
            ctx.accounts.destination.to_account_info(),
            &ctx.accounts.system_program,
            lamports,
            &[vault_seeds],
        )?;
        Ok(())
    }

}

fn split_deposit_lamports(lamports: u64) -> Result<(u64, u64)> {
    let dev_fee = lamports
        .checked_mul(DEV_FEE_BPS)
        .ok_or(ErrorCode::MathOverflow)?
        .checked_div(BPS_DENOMINATOR)
        .ok_or(ErrorCode::MathOverflow)?;
    Ok((lamports.checked_sub(dev_fee).ok_or(ErrorCode::MathOverflow)?, dev_fee))
}

fn main_pool_action_lamports(
    pool_state: &PoolState,
    action: u8,
    units: u16,
    lamports: u64,
) -> Result<u64> {
    match action {
        ACTION_GACHA => {
            require!(
                units >= 1 && units <= MAX_GACHA_ROLLS_PER_IX,
                ErrorCode::BadGachaRolls
            );
            let fee_units = (units as u64)
                .checked_add(GACHA_ROLLS_PER_FEE_UNIT - 1)
                .ok_or(ErrorCode::MathOverflow)?
                .checked_div(GACHA_ROLLS_PER_FEE_UNIT)
                .ok_or(ErrorCode::MathOverflow)?;
            pool_state
                .action_fee_lamports
                .checked_mul(fee_units)
                .ok_or(ErrorCode::MathOverflow.into())
        }
        ACTION_LAB => {
            require!(
                units >= 1 && units <= MAX_LAB_ATTEMPTS as u16,
                ErrorCode::BadAttempts
            );
            pool_state
                .action_fee_lamports
                .checked_mul(units as u64)
                .ok_or(ErrorCode::MathOverflow.into())
        }
        ACTION_CONTRIBUTE => {
            require!(lamports > 0, ErrorCode::ZeroAmount);
            Ok(lamports)
        }
        ACTION_MINING_EXTRA_RIG => Ok(MINING_EXTRA_RIG_SLOT_LAMPORTS),
        ACTION_INVENTORY_SLOT_BUNDLE => Ok(INVENTORY_SLOT_BUNDLE_LAMPORTS),
        ACTION_NFT_BURN => Ok(pool_state.action_fee_lamports),
        ACTION_CITY_UPGRADE => Ok(pool_state.action_fee_lamports),
        ACTION_GUILD_PROJECT_UPGRADE => Ok(pool_state.action_fee_lamports),
        _ => err!(ErrorCode::BadAction),
    }
}

fn transfer_user_to_vault_with_dev_fee<'info>(
    user: &Signer<'info>,
    vault: AccountInfo<'info>,
    dev_fee_recipient: AccountInfo<'info>,
    system_program: &Program<'info, System>,
    lamports: u64,
) -> Result<()> {
    let (pool_lamports, dev_fee_lamports) = split_deposit_lamports(lamports)?;
    if pool_lamports > 0 {
        transfer_user_to_vault(user, vault, system_program, pool_lamports)?;
    }
    if dev_fee_lamports > 0 {
        transfer_user_to_vault(user, dev_fee_recipient, system_program, dev_fee_lamports)?;
    }
    Ok(())
}

fn transfer_user_to_vault<'info>(
    user: &Signer<'info>,
    vault: AccountInfo<'info>,
    system_program: &Program<'info, System>,
    lamports: u64,
) -> Result<()> {
    let cpi = CpiContext::new(
        system_program.to_account_info(),
        system_program::Transfer {
            from: user.to_account_info(),
            to: vault,
        },
    );
    system_program::transfer(cpi, lamports)
}

fn vault_net_lamports(vault: &AccountInfo) -> Result<u64> {
    let rent = Rent::get()?;
    let min = rent.minimum_balance(vault.data_len());
    Ok(vault.lamports().saturating_sub(min))
}

fn transfer_vault_to_dest_signed<'info>(
    vault: AccountInfo<'info>,
    dest: AccountInfo<'info>,
    _system_program: &Program<'info, System>,
    lamports: u64,
    _vault_signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    require!(
        vault_net_lamports(&vault)? >= lamports,
        ErrorCode::InsufficientVaultFunds
    );
    **vault.try_borrow_mut_lamports()? = vault
        .lamports()
        .checked_sub(lamports)
        .ok_or(ErrorCode::MathOverflow)?;
    **dest.try_borrow_mut_lamports()? = dest
        .lamports()
        .checked_add(lamports)
        .ok_or(ErrorCode::MathOverflow)?;
    Ok(())
}

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(
        init,
        payer = authority,
        space = PoolState::SPACE,
        seeds = [b"mirl".as_ref(), b"state".as_ref()],
        bump
    )]
    pub pool_state: Account<'info, PoolState>,
    #[account(
        init,
        payer = authority,
        space = SubVaultMarker::SPACE,
        seeds = [b"mirl".as_ref(), b"vault".as_ref()],
        bump
    )]
    pub pool_vault: Account<'info, SubVaultMarker>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct SetAuthority<'info> {
    #[account(
        mut,
        seeds = [b"mirl".as_ref(), b"state".as_ref()],
        bump = pool_state.bump_state
    )]
    pub pool_state: Account<'info, PoolState>,
    #[account(mut, address = pool_state.authority)]
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct DepositFlat<'info> {
    #[account(mut)]
    pub user: Signer<'info>,
    #[account(
        seeds = [b"mirl".as_ref(), b"state".as_ref()],
        bump = pool_state.bump_state
    )]
    pub pool_state: Account<'info, PoolState>,
    #[account(
        mut,
        seeds = [b"mirl".as_ref(), b"vault".as_ref()],
        bump = pool_state.bump_vault
    )]
    pub pool_vault: Account<'info, SubVaultMarker>,
    /// CHECK: constrained to the recipient stored in PoolState.
    #[account(mut, address = pool_state.dev_fee_recipient)]
    pub dev_fee_recipient: AccountInfo<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct AuthoritySend<'info> {
    #[account(
        seeds = [b"mirl".as_ref(), b"state".as_ref()],
        bump = pool_state.bump_state
    )]
    pub pool_state: Account<'info, PoolState>,
    #[account(mut, address = pool_state.authority)]
    pub authority: Signer<'info>,
    #[account(
        mut,
        seeds = [b"mirl".as_ref(), b"vault".as_ref()],
        bump = pool_state.bump_vault
    )]
    pub pool_vault: Account<'info, SubVaultMarker>,
    /// CHECK: any system account recipient
    #[account(mut)]
    pub destination: AccountInfo<'info>,
    pub system_program: Program<'info, System>,
}




#[derive(Accounts)]
#[instruction(session_key: [u8; 32], lamports: u64)]
pub struct DepositPvpSessionAddon<'info> {
    #[account(mut)]
    pub user: Signer<'info>,
    #[account(
        seeds = [b"mirl".as_ref(), b"state".as_ref()],
        bump = pool_state.bump_state
    )]
    pub pool_state: Account<'info, PoolState>,
    #[account(
        init_if_needed,
        payer = user,
        space = SubVaultMarker::SPACE,
        seeds = [b"mirl".as_ref(), b"pvpaddon".as_ref(), session_key.as_ref()],
        bump
    )]
    pub pvp_addon_vault: Account<'info, SubVaultMarker>,
    /// CHECK: constrained to the recipient stored in PoolState.
    #[account(mut, address = pool_state.dev_fee_recipient)]
    pub dev_fee_recipient: AccountInfo<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(session_key: [u8; 32], lamports: u64)]
pub struct PvpAddonSkimFeeToMain<'info> {
    #[account(
        seeds = [b"mirl".as_ref(), b"state".as_ref()],
        bump = pool_state.bump_state
    )]
    pub pool_state: Account<'info, PoolState>,
    #[account(mut, address = pool_state.authority)]
    pub authority: Signer<'info>,
    #[account(
        mut,
        seeds = [b"mirl".as_ref(), b"vault".as_ref()],
        bump = pool_state.bump_vault
    )]
    pub pool_vault: Account<'info, SubVaultMarker>,
    #[account(
        mut,
        seeds = [b"mirl".as_ref(), b"pvpaddon".as_ref(), session_key.as_ref()],
        bump
    )]
    pub pvp_addon_vault: Account<'info, SubVaultMarker>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(session_key: [u8; 32], lamports: u64)]
pub struct PvpAddonAuthorityWithdraw<'info> {
    #[account(
        seeds = [b"mirl".as_ref(), b"state".as_ref()],
        bump = pool_state.bump_state
    )]
    pub pool_state: Account<'info, PoolState>,
    #[account(mut, address = pool_state.authority)]
    pub authority: Signer<'info>,
    #[account(
        mut,
        seeds = [b"mirl".as_ref(), b"pvpaddon".as_ref(), session_key.as_ref()],
        bump
    )]
    pub pvp_addon_vault: Account<'info, SubVaultMarker>,
    /// CHECK: payout recipient
    #[account(mut)]
    pub destination: AccountInfo<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(session_key: [u8; 32])]
pub struct PvpAddonClose<'info> {
    #[account(
        seeds = [b"mirl".as_ref(), b"state".as_ref()],
        bump = pool_state.bump_state
    )]
    pub pool_state: Account<'info, PoolState>,
    #[account(mut, address = pool_state.authority)]
    pub authority: Signer<'info>,
    #[account(
        mut,
        seeds = [b"mirl".as_ref(), b"pvpaddon".as_ref(), session_key.as_ref()],
        bump,
        close = authority
    )]
    pub pvp_addon_vault: Account<'info, SubVaultMarker>,
    pub system_program: Program<'info, System>,
}



#[error_code]
pub enum ErrorCode {
    #[msg("attempts must be 1..15")]
    BadAttempts,
    #[msg("gacha rolls must be 1..2000")]
    BadGachaRolls,
    #[msg("math overflow")]
    MathOverflow,
    #[msg("zero amount")]
    ZeroAmount,
    #[msg("dev fee recipient cannot be default pubkey")]
    BadDevRecipient,
    #[msg("authority cannot be default pubkey")]
    BadAuthority,
    #[msg("pvp: vault must be empty (only rent) before close")]
    PvpVaultNotEmpty,
    #[msg("pvp: PvP vault balance too low for fee skim")]
    PvpVaultInsufficientForFeeSkim,
    #[msg("vault balance is too low")]
    InsufficientVaultFunds,
    #[msg("unknown main-pool deposit action")]
    BadAction,
}

