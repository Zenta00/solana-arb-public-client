use std::error::Error;
use std::fmt;

#[derive(Debug, PartialEq)]
pub enum LBError {
    InvalidStartBinIndex,

    InvalidBinId,

    InvalidInput,

    ExceededAmountSlippageTolerance,

    ExceededBinSlippageTolerance,

    CompositionFactorFlawed,

    NonPresetBinStep,

    ZeroLiquidity,

    InvalidPosition,

    BinArrayNotFound,

    InvalidTokenMint,

    InvalidAccountForSingleDeposit,

    PairInsufficientLiquidity,

    InvalidFeeOwner,

    InvalidFeeWithdrawAmount,

    InvalidAdmin,

    IdenticalFeeOwner,

    InvalidBps,

    MathOverflow,

    TypeCastFailed,

    InvalidRewardIndex,

    InvalidRewardDuration,

    RewardInitialized,

    RewardUninitialized,

    IdenticalFunder,

    RewardCampaignInProgress,

    IdenticalRewardDuration,

    InvalidBinArray,

    NonContinuousBinArrays,

    InvalidRewardVault,

    NonEmptyPosition,

    UnauthorizedAccess,

    InvalidFeeParameter,

    MissingOracle,

    InsufficientSample,

    InvalidLookupTimestamp,

    BitmapExtensionAccountIsNotProvided,

    CannotFindNonZeroLiquidityBinArrayId,

    BinIdOutOfBound,

    InsufficientOutAmount,

    InvalidPositionWidth,

    ExcessiveFeeUpdate,

    PoolDisabled,

    InvalidPoolType,

    ExceedMaxWhitelist,

    InvalidIndex,

    RewardNotEnded,

    MustWithdrawnIneligibleReward,

    UnauthorizedAddress,

    OperatorsAreTheSame,

    WithdrawToWrongTokenAccount,

    WrongRentReceiver,

    AlreadyPassActivationPoint,

    ExceedMaxSwappedAmount,

    InvalidStrategyParameters,

    LiquidityLocked,

    BinRangeIsNotEmpty,

    NotExactAmountOut,

    InvalidActivationType,
}

impl fmt::Display for LBError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl Error for LBError {}
