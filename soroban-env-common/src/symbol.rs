//! Ths `Symbol` type is designed for encoding short, unambiguous, single-word
//! identifiers such as the names of contract functions or assets in the
//! network. `Symbol`s only admit characters from the 63-character repertoire
//! `[a-zA-Z0-9_]` -- latin-script alphabetic letters, digits, and underscores.
//!
//! There are two reasons for this type to be different from the general
//! `String` type:
//!
//!   1. We provide a space-optimized "small" form (`SymbolSmall`) that uses
//!      small 6-bit codes (since the character repertoire is only 63 characters
//!      plus null) and bit-packs them into the body of a `Val` such that they
//!      can be used to represent short identifiers (up to 9 characters long)
//!      without allocating a host object at all, essentially as "machine
//!      integers with a textual interpretation". This is an optimization, since
//!      we expect contracts to use `Symbol`s heavily.
//!
//!   2. We expect (though do not require) `String` to usually be interpreted as
//!      Unicode codepoints encoded in UTF-8. Unicode characters unfortunately
//!      admit a wide variety of "confusables" or "homoglyphs": characters that
//!      have different codes, but look the same when rendered in many common
//!      fonts. In many contexts these represent a significant security risk to
//!      end users -- for example by confusing an asset named `USD` (encoded as
//!      the UTF-8 hex byte sequence `[55 53 44]`) with a similar-looking but
//!      different asset named `ՍЅᎠ` (encoded as `[d5 8d d0 85 e1 8e a0]`) --
//!      and so we provide `Symbol` as an alternative to `String` for use in
//!      contexts where users wish to minimize such risks, by restricting the
//!      possible characters that can occur.
//!
//! `SymbolSmall` values are packed into a 56 bits (the "body" part of a 64-bit
//! `Val` word) with zero padding in the high-order bits rather than low-order
//! bits. While this means that lexicographical ordering of `SymbolSmall` values
//! does not coincide with simple integer ordering of `Val` bodies, it optimizes
//! the space cost of `SymbolSmall` literals in WASM bytecode, where all integer
//! literals are encoded as variable-length little-endian values, using ULEB128.

use crate::{
    declare_tag_based_small_and_object_wrappers, require, val::ValConvert, Compare,
    ConversionError, Env, Tag, TryFromVal, Val,
};
use core::{cmp::Ordering, fmt::Debug, hash::Hash, str};

declare_tag_based_small_and_object_wrappers!(Symbol, SymbolSmall, SymbolObject);

/// Errors related to operations on the [SymbolObject] and [SymbolSmall] types.
#[derive(Debug)]
pub enum SymbolError {
    /// Returned when attempting to form a [SymbolSmall] from a string with more
    /// than 9 characters.
    TooLong(usize),
    /// Returned when attempting to form a [SymbolObject] or [SymbolSmall] from
    /// a string with characters outside the range `[a-zA-Z0-9_]`.
    BadChar(char),
}

impl core::fmt::Display for SymbolError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SymbolError::TooLong(len) => f.write_fmt(format_args!(
                "symbol too long: length {len}, max {MAX_SMALL_CHARS}"
            )),
            SymbolError::BadChar(char) => f.write_fmt(format_args!(
                "symbol bad char: encountered {char}, supported range [a-zA-Z0-9_]"
            )),
        }
    }
}

impl From<SymbolError> for ConversionError {
    fn from(_: SymbolError) -> Self {
        ConversionError
    }
}

extern crate static_assertions as sa;

use super::val::BODY_BITS;

// Small symbols admit 9 6-bit chars for 54 bits.

pub(crate) const MAX_SMALL_CHARS: usize = 9;
const CODE_BITS: usize = 6;
const CODE_MASK: u64 = (1u64 << CODE_BITS) - 1;
sa::const_assert!(CODE_MASK == 0x3f);
sa::const_assert!(CODE_BITS * MAX_SMALL_CHARS + 2 == BODY_BITS);

impl<E: Env> TryFromVal<E, &str> for Symbol {
    type Error = crate::Error;

    fn try_from_val(env: &E, v: &&str) -> Result<Self, Self::Error> {
        if let Ok(ss) = SymbolSmall::try_from_str(v) {
            Ok(Self(ss.0))
        } else {
            let sobj = env.symbol_new_from_slice(v).map_err(Into::into)?;
            Ok(Self(sobj.0))
        }
    }
}

impl<E: Env> TryFromVal<E, &[u8]> for Symbol {
    type Error = crate::Error;

    fn try_from_val(env: &E, v: &&[u8]) -> Result<Self, Self::Error> {
        // We don't know this byte-slice is actually utf-8 ...
        let s: &str = unsafe { core::str::from_utf8_unchecked(v) };
        // ... but this next conversion step will check that its
        // _bytes_ are in the symbol-char range, which is a subset
        // of utf-8, so we're only lying harmlessly.
        Symbol::try_from_val(env, &s)
    }
}

impl<E: Env> Compare<Symbol> for E {
    type Error = E::Error;
    fn compare(&self, a: &Symbol, b: &Symbol) -> Result<Ordering, Self::Error> {
        let taga = a.0.get_tag();
        let tagb = b.0.get_tag();
        match taga.cmp(&tagb) {
            Ordering::Equal => {
                if taga == Tag::SymbolSmall {
                    let ssa = unsafe { SymbolSmall::unchecked_from_val(a.0) };
                    let ssb = unsafe { SymbolSmall::unchecked_from_val(b.0) };
                    Ok(ssa.cmp(&ssb))
                } else {
                    let soa = unsafe { SymbolObject::unchecked_from_val(a.0) };
                    let sob = unsafe { SymbolObject::unchecked_from_val(b.0) };
                    self.compare(&soa, &sob)
                }
            }
            other => Ok(other),
        }
    }
}

impl Symbol {
    #[doc(hidden)]
    pub const unsafe fn from_small_body(body: u64) -> Self {
        // Can't panic, though it's possible you gave it a weird
        // symbol, every possible bit-pattern in the low 56 bits
        // is a valid small symbol.
        Symbol(SymbolSmall::from_body(body).0)
    }

    pub const fn try_from_small_str(s: &str) -> Result<Self, SymbolError> {
        match SymbolSmall::try_from_str(s) {
            Ok(ss) => Ok(Symbol(ss.0)),
            Err(e) => Err(e),
        }
    }

    // This should not be generally available as it can easily panic.
    #[cfg(feature = "testutils")]
    pub const fn from_small_str(s: &str) -> Self {
        Symbol(SymbolSmall::from_str(s).0)
    }
}

impl Ord for SymbolSmall {
    fn cmp(&self, other: &Self) -> Ordering {
        Iterator::cmp(self.into_iter(), *other)
    }
}

impl TryFrom<&[u8]> for SymbolSmall {
    type Error = SymbolError;

    fn try_from(b: &[u8]) -> Result<SymbolSmall, SymbolError> {
        Self::try_from_bytes(b)
    }
}

#[cfg(feature = "std")]
use stellar_xdr::StringM;
use stellar_xdr::SCSYMBOL_LIMIT;
#[cfg(feature = "std")]
impl<const N: u32> TryFrom<StringM<N>> for SymbolSmall {
    type Error = SymbolError;

    fn try_from(v: StringM<N>) -> Result<Self, Self::Error> {
        v.as_slice().try_into()
    }
}
#[cfg(feature = "std")]
impl<const N: u32> TryFrom<&StringM<N>> for SymbolSmall {
    type Error = SymbolError;

    fn try_from(v: &StringM<N>) -> Result<Self, Self::Error> {
        v.as_slice().try_into()
    }
}

impl SymbolSmall {
    #[doc(hidden)]
    pub const fn validate_char(ch: char) -> Result<(), SymbolError> {
        match SymbolSmall::encode_char(ch) {
            Ok(_) => Ok(()),
            Err(e) => Err(e),
        }
    }

    const fn encode_char(ch: char) -> Result<u64, SymbolError> {
        let v = match ch {
            '_' => 1,
            '0'..='9' => 2 + ((ch as u64) - ('0' as u64)),
            'A'..='Z' => 12 + ((ch as u64) - ('A' as u64)),
            'a'..='z' => 38 + ((ch as u64) - ('a' as u64)),
            _ => return Err(SymbolError::BadChar(ch)),
        };
        Ok(v)
    }

    pub const fn try_from_bytes(b: &[u8]) -> Result<SymbolSmall, SymbolError> {
        let mut n = 0;
        let mut accum: u64 = 0;
        while n < b.len() {
            let ch = b[n] as char;
            if n >= MAX_SMALL_CHARS {
                return Err(SymbolError::TooLong(b.len()));
            }
            n += 1;
            accum <<= CODE_BITS;
            let v = match SymbolSmall::encode_char(ch) {
                Ok(v) => v,
                Err(e) => return Err(e),
            };
            accum |= v;
        }
        Ok(unsafe { Self::from_body(accum) })
    }

    pub const fn try_from_str(s: &str) -> Result<SymbolSmall, SymbolError> {
        Self::try_from_bytes(s.as_bytes())
    }

    #[doc(hidden)]
    pub const unsafe fn get_body(&self) -> u64 {
        self.0.get_body()
    }

    // This should not be generally available as it can easily panic.
    #[cfg(feature = "testutils")]
    pub const fn from_str(s: &str) -> SymbolSmall {
        match Self::try_from_str(s) {
            Ok(sym) => sym,
            Err(SymbolError::TooLong(_)) => panic!("symbol too long"),
            Err(SymbolError::BadChar(_)) => panic!("symbol bad char"),
        }
    }

    pub fn to_str(&self) -> SymbolStr {
        let mut chars = [b'\x00'; SCSYMBOL_LIMIT as usize];
        for (i, ch) in self.into_iter().enumerate() {
            chars[i] = ch as u8;
        }
        SymbolStr(chars)
    }
}

/// An expanded form of a [Symbol] that stores its characters as ASCII-range
/// bytes in a [u8] array -- up to the maximum size of a large symbol object --
/// rather than as packed 6-bit codes within a [u64]. Useful for interoperation
/// with standard Rust string types.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct SymbolStr([u8; SCSYMBOL_LIMIT as usize]);

impl SymbolStr {
    pub fn is_empty(&self) -> bool {
        self.0[0] == 0
    }
    pub fn len(&self) -> usize {
        let s: &[u8] = &self.0;
        for (i, x) in s.iter().enumerate() {
            if *x == 0 {
                return i;
            }
        }
        s.len()
    }
}

impl Debug for SymbolStr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s: &str = self.as_ref();
        f.debug_tuple("SymbolStr").field(&s).finish()
    }
}

impl AsRef<[u8]> for SymbolStr {
    fn as_ref(&self) -> &[u8] {
        let s: &[u8] = &self.0;
        &s[..self.len()]
    }
}

impl AsRef<str> for SymbolStr {
    fn as_ref(&self) -> &str {
        let s: &[u8] = self.as_ref();
        unsafe { str::from_utf8_unchecked(s) }
    }
}

impl From<&SymbolSmall> for SymbolStr {
    fn from(s: &SymbolSmall) -> Self {
        s.to_str()
    }
}

impl From<SymbolSmall> for SymbolStr {
    fn from(s: SymbolSmall) -> Self {
        (&s).into()
    }
}

impl<E: Env> TryFromVal<E, Symbol> for SymbolStr {
    type Error = crate::Error;

    fn try_from_val(env: &E, v: &Symbol) -> Result<Self, Self::Error> {
        if let Ok(ss) = SymbolSmall::try_from(*v) {
            Ok(ss.into())
        } else {
            let obj: SymbolObject = unsafe { SymbolObject::unchecked_from_val(v.0) };
            let mut arr = [0u8; SCSYMBOL_LIMIT as usize];
            env.symbol_copy_to_slice(obj, Val::U32_ZERO, &mut arr)
                .map_err(Into::into)?;
            Ok(SymbolStr(arr))
        }
    }
}

#[cfg(feature = "std")]
use std::string::{String, ToString};
#[cfg(feature = "std")]
impl From<SymbolSmall> for String {
    fn from(s: SymbolSmall) -> Self {
        s.to_string()
    }
}
#[cfg(feature = "std")]
impl From<SymbolStr> for String {
    fn from(s: SymbolStr) -> Self {
        s.to_string()
    }
}
#[cfg(feature = "std")]
impl ToString for SymbolSmall {
    fn to_string(&self) -> String {
        self.into_iter().collect()
    }
}
#[cfg(feature = "std")]
impl ToString for SymbolStr {
    fn to_string(&self) -> String {
        let s: &str = self.as_ref();
        s.to_string()
    }
}

impl IntoIterator for SymbolSmall {
    type Item = char;
    type IntoIter = SymbolSmallIter;
    fn into_iter(self) -> Self::IntoIter {
        SymbolSmallIter(self.as_val().get_body())
    }
}

/// An iterator that decodes the individual bit-packed characters from a
/// symbol and yields them as regular Rust [char] values.
#[repr(transparent)]
#[derive(Clone)]
pub struct SymbolSmallIter(u64);

impl Iterator for SymbolSmallIter {
    type Item = char;

    fn next(&mut self) -> Option<Self::Item> {
        while self.0 != 0 {
            let res = match ((self.0 >> ((MAX_SMALL_CHARS - 1) * CODE_BITS)) & CODE_MASK) as u8 {
                1 => b'_',
                n @ (2..=11) => b'0' + n - 2,
                n @ (12..=37) => b'A' + n - 12,
                n @ (38..=63) => b'a' + n - 38,
                _ => b'\0',
            };
            self.0 <<= CODE_BITS;
            if res != b'\0' {
                return Some(res as char);
            }
        }
        None
    }
}

impl FromIterator<char> for SymbolSmall {
    fn from_iter<T: IntoIterator<Item = char>>(iter: T) -> Self {
        let mut accum: u64 = 0;
        for (n, i) in iter.into_iter().enumerate() {
            require(n < MAX_SMALL_CHARS);
            accum <<= CODE_BITS;
            let v = match i {
                '_' => 1,
                '0'..='9' => 2 + ((i as u64) - ('0' as u64)),
                'A'..='Z' => 12 + ((i as u64) - ('A' as u64)),
                'a'..='z' => 38 + ((i as u64) - ('a' as u64)),
                _ => break,
            };
            accum |= v;
        }
        unsafe { Self::from_body(accum) }
    }
}

#[cfg(feature = "std")]
use crate::xdr::{ScSymbol, ScVal};

#[cfg(feature = "std")]
impl TryFrom<ScVal> for SymbolSmall {
    type Error = crate::Error;
    fn try_from(v: ScVal) -> Result<Self, Self::Error> {
        (&v).try_into()
    }
}
#[cfg(feature = "std")]
impl TryFrom<&ScVal> for SymbolSmall {
    type Error = crate::Error;
    fn try_from(v: &ScVal) -> Result<Self, Self::Error> {
        if let ScVal::Symbol(crate::xdr::ScSymbol(vec)) = v {
            vec.try_into().map_err(Into::into)
        } else {
            Err(ConversionError.into())
        }
    }
}

#[cfg(feature = "std")]
impl<E: Env> TryFromVal<E, ScVal> for Symbol {
    type Error = crate::Error;

    fn try_from_val(env: &E, v: &ScVal) -> Result<Self, Self::Error> {
        Symbol::try_from_val(env, &v)
    }
}

#[cfg(feature = "std")]
impl<E: Env> TryFromVal<E, &ScVal> for Symbol {
    type Error = crate::Error;
    fn try_from_val(env: &E, v: &&ScVal) -> Result<Self, Self::Error> {
        if let ScVal::Symbol(sym) = v {
            Symbol::try_from_val(env, &sym)
        } else {
            Err(ConversionError.into())
        }
    }
}

#[cfg(feature = "std")]
impl<E: Env> TryFromVal<E, ScSymbol> for Symbol {
    type Error = crate::Error;

    fn try_from_val(env: &E, v: &ScSymbol) -> Result<Self, Self::Error> {
        Symbol::try_from_val(env, &v)
    }
}

#[cfg(feature = "std")]
impl<E: Env> TryFromVal<E, &ScSymbol> for Symbol {
    type Error = crate::Error;
    fn try_from_val(env: &E, v: &&ScSymbol) -> Result<Self, Self::Error> {
        Symbol::try_from_val(env, &v.0.as_slice())
    }
}

#[cfg(feature = "std")]
impl TryFrom<SymbolSmall> for ScVal {
    type Error = crate::Error;
    fn try_from(s: SymbolSmall) -> Result<Self, crate::Error> {
        let res: Result<Vec<u8>, _> = s.into_iter().map(<u8 as TryFrom<char>>::try_from).collect();
        let vec = res.map_err(|_| {
            crate::Error::from_type_and_code(
                crate::xdr::ScErrorType::Value,
                crate::xdr::ScErrorCode::InvalidInput,
            )
        })?;
        Ok(ScVal::Symbol(vec.try_into()?))
    }
}

#[cfg(feature = "std")]
impl TryFrom<&SymbolSmall> for ScVal {
    type Error = crate::Error;
    fn try_from(s: &SymbolSmall) -> Result<Self, crate::Error> {
        (*s).try_into()
    }
}

#[cfg(feature = "std")]
impl<E: Env> TryFromVal<E, Symbol> for ScVal {
    type Error = crate::Error;
    fn try_from_val(e: &E, s: &Symbol) -> Result<Self, crate::Error> {
        Ok(ScVal::Symbol(ScSymbol::try_from_val(e, s)?))
    }
}

#[cfg(feature = "std")]
impl<E: Env> TryFromVal<E, Symbol> for ScSymbol {
    type Error = crate::Error;
    fn try_from_val(e: &E, s: &Symbol) -> Result<Self, crate::Error> {
        let sstr = SymbolStr::try_from_val(e, s)?;
        Ok(ScSymbol(sstr.0.as_slice()[0..sstr.len()].try_into()?))
    }
}

#[cfg(test)]
mod test_without_string {
    use super::{SymbolSmall, SymbolStr};

    #[test]
    fn test_roundtrip() {
        let input = "stellar";
        let sym = SymbolSmall::try_from_str(input).unwrap();
        let sym_str = SymbolStr::from(sym);
        let s: &str = sym_str.as_ref();
        assert_eq!(s, input);
    }

    #[test]
    fn test_roundtrip_zero() {
        let input = "";
        let sym = SymbolSmall::try_from_str(input).unwrap();
        let sym_str = SymbolStr::from(sym);
        let s: &str = sym_str.as_ref();
        assert_eq!(s, input);
    }

    #[test]
    fn test_roundtrip_nine() {
        let input = "123456789";
        let sym = SymbolSmall::try_from_str(input).unwrap();
        let sym_str = SymbolStr::from(sym);
        let s: &str = sym_str.as_ref();
        assert_eq!(s, input);
    }

    #[test]
    fn test_enc() {
        // Some exact test vectors to ensure the encoding is what we expect.
        let vectors: &[(&str, u64)] = &[
            ("a",           0b__000_000__000_000__000_000__000_000__000_000__000_000__000_000__000_000__100_110_u64),
            ("ab",          0b__000_000__000_000__000_000__000_000__000_000__000_000__000_000__100_110__100_111_u64),
            ("abc",         0b__000_000__000_000__000_000__000_000__000_000__000_000__100_110__100_111__101_000_u64),
            ("ABC",         0b__000_000__000_000__000_000__000_000__000_000__000_000__001_100__001_101__001_110_u64),
            ("____5678",    0b__000_000__000_001__000_001__000_001__000_001__000_111__001_000__001_001__001_010_u64),
            ("____56789",   0b__000_001__000_001__000_001__000_001__000_111__001_000__001_001__001_010__001_011_u64),
        ];
        for (s, body) in vectors.iter() {
            let sym = SymbolSmall::try_from_str(s).unwrap();
            assert_eq!(unsafe { sym.get_body() }, *body);
        }
    }

    #[test]
    fn test_ord() {
        let vals = ["Hello", "hello", "hellos", "", "_________", "________"];
        for a in vals.iter() {
            let a_sym = SymbolSmall::try_from_str(a).unwrap();
            for b in vals.iter() {
                let b_sym = SymbolSmall::try_from_str(b).unwrap();
                assert_eq!(a.cmp(b), a_sym.cmp(&b_sym));
            }
        }
    }
}

#[cfg(all(test, feature = "std"))]
mod test_with_string {
    use super::SymbolSmall;
    use std::string::{String, ToString};

    #[test]
    fn test_roundtrip() {
        let input = "stellar";
        let sym = SymbolSmall::try_from_str(input).unwrap();
        let s: String = sym.to_string();
        assert_eq!(input, &s);
    }

    #[test]
    fn test_roundtrip_zero() {
        let input = "";
        let sym = SymbolSmall::try_from_str(input).unwrap();
        let s: String = sym.to_string();
        assert_eq!(input, &s);
    }

    #[test]
    fn test_roundtrip_nine() {
        let input = "123456789";
        let sym = SymbolSmall::try_from_str(input).unwrap();
        let s: String = sym.to_string();
        assert_eq!(input, &s);
    }
}
