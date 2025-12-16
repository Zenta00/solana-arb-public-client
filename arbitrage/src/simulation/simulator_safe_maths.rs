use std::error::Error;
use std::fmt;

#[derive(Debug)]
pub enum SafeMathError {
    Overflow,
    Underflow,
    DivisionByZero,
    Undefined
}

impl fmt::Display for SafeMathError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            SafeMathError::Overflow => write!(f, "Math operation resulted in overflow"),
            SafeMathError::Underflow => write!(f, "Math operation resulted in underflow"),
            SafeMathError::DivisionByZero => write!(f, "Division by zero"),
            SafeMathError::Undefined => write!(f, "Operation is undefined"),
        }
    }
}

impl Error for SafeMathError {}

pub trait SafeMath {
    fn safe_mul(self, other: Self) -> Result<Self, SafeMathError> where Self: Sized;
    fn safe_sub(self, other: Self) -> Result<Self, SafeMathError> where Self: Sized;
    fn safe_div(self, other: Self) -> Result<Self, SafeMathError> where Self: Sized;
    fn safe_add(self, other:Self) -> Result<Self, SafeMathError> where Self: Sized;
    fn inv_price(self) -> Result<Self, SafeMathError> where Self: Sized {
        return Err(SafeMathError::Undefined);
    }
}

impl SafeMath for u64 {
    fn safe_mul(self, other: Self) -> Result<Self, SafeMathError> {
        self.checked_mul(other).ok_or(SafeMathError::Overflow)
    }

    fn safe_add(self, other: Self) -> Result<Self, SafeMathError> {
        self.checked_add(other).ok_or(SafeMathError::Overflow)
    }

    fn safe_sub(self, other: Self) -> Result<Self, SafeMathError> {
        self.checked_sub(other).ok_or(SafeMathError::Underflow)
    }

    fn safe_div(self, other: Self) -> Result<Self, SafeMathError> {
        if other == 0 {
            return Err(SafeMathError::DivisionByZero);
        }
        self.checked_div(other).ok_or(SafeMathError::Overflow)
    }
}

impl SafeMath for u128 {
    fn safe_mul(self, other: Self) -> Result<Self, SafeMathError> {
        self.checked_mul(other).ok_or(SafeMathError::Overflow)
    }

    fn safe_add(self, other: Self) -> Result<Self, SafeMathError> {
        self.checked_add(other).ok_or(SafeMathError::Overflow)
    }

    fn safe_sub(self, other: Self) -> Result<Self, SafeMathError> {
        self.checked_sub(other).ok_or(SafeMathError::Underflow)
    }

    fn safe_div(self, other: Self) -> Result<Self, SafeMathError> {
        if other == 0 {
            return Err(SafeMathError::DivisionByZero);
        }
        self.checked_div(other).ok_or(SafeMathError::Overflow)
    }
    
    fn inv_price(self) -> Result<Self, SafeMathError> {
        if self == 0 {
            return Ok(0);
        }
        u128::MAX.checked_div(self).ok_or(SafeMathError::Overflow)
    }
}

impl SafeMath for i32 {
    fn safe_mul(self, other: Self) -> Result<Self, SafeMathError> {
        self.checked_mul(other).ok_or(SafeMathError::Overflow)
    }

    fn safe_sub(self, other: Self) -> Result<Self, SafeMathError> {
        self.checked_sub(other).ok_or(SafeMathError::Underflow)
    }

    fn safe_add(self, other: Self) -> Result<Self, SafeMathError> {
        self.checked_add(other).ok_or(SafeMathError::Overflow)
    }

    fn safe_div(self, other: Self) -> Result<Self, SafeMathError> {
        if other == 0 {
            return Err(SafeMathError::DivisionByZero);
        }
        self.checked_div(other).ok_or(SafeMathError::Overflow)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_u64_safe_mul() {
        assert_eq!(5u64.safe_mul(5).unwrap(), 25);
        assert!(u64::MAX.safe_mul(2).is_err());
    }

    #[test]
    fn test_u64_safe_sub() {
        assert_eq!(10u64.safe_sub(5).unwrap(), 5);
        assert!(5u64.safe_sub(10).is_err());
    }

    #[test]
    fn test_u64_safe_div() {
        assert_eq!(10u64.safe_div(2).unwrap(), 5);
        assert!(10u64.safe_div(0).is_err());
    }

    #[test]
    fn test_u128_safe_mul() {
        assert_eq!(5u128.safe_mul(5).unwrap(), 25);
        assert!(u128::MAX.safe_mul(2).is_err());
    }

    #[test]
    fn test_u128_safe_sub() {
        assert_eq!(10u128.safe_sub(5).unwrap(), 5);
        assert!(5u128.safe_sub(10).is_err());
    }

    #[test]
    fn test_u128_safe_div() {
        assert_eq!(10u128.safe_div(2).unwrap(), 5);
        assert!(10u128.safe_div(0).is_err());
    }
}
