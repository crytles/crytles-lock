use anchor_lang::prelude::*;
use anchor_lang::system_program;
use anchor_spl::{
    associated_token::AssociatedToken,
    token::{self, CloseAccount, Mint, Token, TokenAccount, TransferChecked},
};

declare_id!("CVMSSc2XeCUuaqYXq7hVmxr2BK7kTATcrtWmM7CwMYGY");

/// Where the service fee goes. Fixed in the code, nobody can change it.
pub const TREASURY: Pubkey = pubkey!("FbKYSHTcZ2ynq9ybNVWYjrTs4fAuyxruMM7aqCSCKWmX");

/// Service fee for creating one lock, in lamports. 0.08 SOL.
pub const FEE_LAMPORTS: u64 = 80_000_000;

/// Longest a lock can run, ten years in seconds.
pub const MAX_LOCK_SECONDS: i64 = 315_360_000;

#[program]
pub mod crytles_lock {
    use super::*;

    /// Lock `amount` of a token until `unlock_time`.
    /// The fee is paid once, here, and never again.
    pub fn lock(
        ctx: Context<LockTokens>,
        lock_id: u64,
        amount: u64,
        unlock_time: i64,
    ) -> Result<()> {
        require!(amount > 0, LockError::ZeroAmount);

        let now = Clock::get()?.unix_timestamp;
        require!(unlock_time > now, LockError::UnlockInThePast);
        require!(
            unlock_time <= now + MAX_LOCK_SECONDS,
            LockError::TooFarInTheFuture
        );

        // service fee
        system_program::transfer(
            CpiContext::new(
                ctx.accounts.system_program.key(),
                system_program::Transfer {
                    from: ctx.accounts.creator.to_account_info(),
                    to: ctx.accounts.treasury.to_account_info(),
                },
            ),
            FEE_LAMPORTS,
        )?;

        // move the tokens into the vault
        token::transfer_checked(
            CpiContext::new(
                ctx.accounts.token_program.key(),
                TransferChecked {
                    from: ctx.accounts.from.to_account_info(),
                    mint: ctx.accounts.mint.to_account_info(),
                    to: ctx.accounts.vault.to_account_info(),
                    authority: ctx.accounts.creator.to_account_info(),
                },
            ),
            amount,
            ctx.accounts.mint.decimals,
        )?;

        let lock = &mut ctx.accounts.lock;
        lock.creator = ctx.accounts.creator.key();
        lock.owner = ctx.accounts.creator.key();
        lock.mint = ctx.accounts.mint.key();
        lock.amount = amount;
        lock.unlock_time = unlock_time;
        lock.locked_at = now;
        lock.lock_id = lock_id;
        lock.bump = ctx.bumps.lock;

        emit!(Locked {
            lock: lock.key(),
            owner: lock.owner,
            mint: lock.mint,
            amount,
            unlock_time,
        });

        Ok(())
    }

    /// Push the unlock time further out. Never closer. No extra fee.
    pub fn extend(ctx: Context<Manage>, new_unlock_time: i64) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        let lock = &mut ctx.accounts.lock;
        require!(
            new_unlock_time > lock.unlock_time,
            LockError::NotLaterThanCurrent
        );
        require!(
            new_unlock_time <= now + MAX_LOCK_SECONDS,
            LockError::TooFarInTheFuture
        );
        lock.unlock_time = new_unlock_time;

        emit!(Extended {
            lock: lock.key(),
            unlock_time: new_unlock_time,
        });

        Ok(())
    }

    /// Hand the locked position to another wallet. The unlock time does not move.
    pub fn transfer_lock(ctx: Context<Manage>, new_owner: Pubkey) -> Result<()> {
        require!(new_owner != Pubkey::default(), LockError::BadOwner);
        let lock = &mut ctx.accounts.lock;
        let old = lock.owner;
        lock.owner = new_owner;

        emit!(Transferred {
            lock: lock.key(),
            from: old,
            to: new_owner,
        });

        Ok(())
    }

    /// Take the tokens back once the time has passed.
    pub fn withdraw(ctx: Context<Withdraw>) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        require!(
            now >= ctx.accounts.lock.unlock_time,
            LockError::StillLocked
        );

        let amount = ctx.accounts.vault.amount;
        require!(amount > 0, LockError::ZeroAmount);

        let creator = ctx.accounts.lock.creator;
        let id = ctx.accounts.lock.lock_id.to_le_bytes();
        let bump = ctx.accounts.lock.bump;
        let seeds: &[&[u8]] = &[b"lock", creator.as_ref(), id.as_ref(), &[bump]];
        let signer: &[&[&[u8]]] = &[seeds];

        token::transfer_checked(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.key(),
                TransferChecked {
                    from: ctx.accounts.vault.to_account_info(),
                    mint: ctx.accounts.mint.to_account_info(),
                    to: ctx.accounts.to.to_account_info(),
                    authority: ctx.accounts.lock.to_account_info(),
                },
                signer,
            ),
            amount,
            ctx.accounts.mint.decimals,
        )?;

        // give the vault rent back to the owner
        token::close_account(CpiContext::new_with_signer(
            ctx.accounts.token_program.key(),
            CloseAccount {
                account: ctx.accounts.vault.to_account_info(),
                destination: ctx.accounts.owner.to_account_info(),
                authority: ctx.accounts.lock.to_account_info(),
            },
            signer,
        ))?;

        emit!(Withdrawn {
            lock: ctx.accounts.lock.key(),
            owner: ctx.accounts.owner.key(),
            amount,
        });

        Ok(())
    }
}

#[derive(Accounts)]
#[instruction(lock_id: u64)]
pub struct LockTokens<'info> {
    #[account(mut)]
    pub creator: Signer<'info>,

    /// CHECK: fixed address, checked below
    #[account(mut, address = TREASURY)]
    pub treasury: UncheckedAccount<'info>,

    pub mint: Account<'info, Mint>,

    #[account(
        mut,
        constraint = from.mint == mint.key() @ LockError::WrongMint,
        constraint = from.owner == creator.key() @ LockError::WrongOwner,
    )]
    pub from: Account<'info, TokenAccount>,

    #[account(
        init,
        payer = creator,
        space = 8 + Lock::SIZE,
        seeds = [b"lock", creator.key().as_ref(), &lock_id.to_le_bytes()],
        bump
    )]
    pub lock: Account<'info, Lock>,

    #[account(
        init,
        payer = creator,
        associated_token::mint = mint,
        associated_token::authority = lock,
    )]
    pub vault: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Manage<'info> {
    pub owner: Signer<'info>,

    #[account(
        mut,
        has_one = owner @ LockError::WrongOwner,
        seeds = [b"lock", lock.creator.as_ref(), &lock.lock_id.to_le_bytes()],
        bump = lock.bump
    )]
    pub lock: Account<'info, Lock>,
}

#[derive(Accounts)]
pub struct Withdraw<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(
        mut,
        has_one = owner @ LockError::WrongOwner,
        has_one = mint @ LockError::WrongMint,
        close = owner,
        seeds = [b"lock", lock.creator.as_ref(), &lock.lock_id.to_le_bytes()],
        bump = lock.bump
    )]
    pub lock: Account<'info, Lock>,

    pub mint: Account<'info, Mint>,

    #[account(
        mut,
        associated_token::mint = mint,
        associated_token::authority = lock,
    )]
    pub vault: Account<'info, TokenAccount>,

    #[account(
        init_if_needed,
        payer = owner,
        associated_token::mint = mint,
        associated_token::authority = owner,
    )]
    pub to: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

#[account]
pub struct Lock {
    pub creator: Pubkey,
    pub owner: Pubkey,
    pub mint: Pubkey,
    pub amount: u64,
    pub unlock_time: i64,
    pub locked_at: i64,
    pub lock_id: u64,
    pub bump: u8,
}

impl Lock {
    pub const SIZE: usize = 32 + 32 + 32 + 8 + 8 + 8 + 8 + 1;
}

#[event]
pub struct Locked {
    pub lock: Pubkey,
    pub owner: Pubkey,
    pub mint: Pubkey,
    pub amount: u64,
    pub unlock_time: i64,
}

#[event]
pub struct Extended {
    pub lock: Pubkey,
    pub unlock_time: i64,
}

#[event]
pub struct Transferred {
    pub lock: Pubkey,
    pub from: Pubkey,
    pub to: Pubkey,
}

#[event]
pub struct Withdrawn {
    pub lock: Pubkey,
    pub owner: Pubkey,
    pub amount: u64,
}

#[error_code]
pub enum LockError {
    #[msg("Amount must be above zero")]
    ZeroAmount,
    #[msg("Unlock time is already in the past")]
    UnlockInThePast,
    #[msg("The new unlock time must be later than the current one")]
    NotLaterThanCurrent,
    #[msg("Tokens are still locked")]
    StillLocked,
    #[msg("Wrong owner")]
    WrongOwner,
    #[msg("Wrong mint")]
    WrongMint,
    #[msg("Bad owner address")]
    BadOwner,
    #[msg("A lock cannot run longer than ten years")]
    TooFarInTheFuture,
}
